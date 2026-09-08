use self::{associations::SchemaAssociations, builtins::builtin_schema, cache::Cache};
use crate::{environment::Environment, util::ArcHashValue, LruCache};
use anyhow::{anyhow, Context};
use async_recursion::async_recursion;
use futures::{stream::FuturesUnordered, StreamExt};
use itertools::Itertools;
use json_value_merge::Merge;
use jsonschema::{error::ValidationErrorKind, Draft, JSONSchema, SchemaResolver, ValidationError};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value;
use std::{borrow::Cow, num::NonZeroUsize, sync::Arc};
use taplo::{
    dom::{self, node::Key, KeyOrIndex, Keys},
    rowan::TextRange,
};
use thiserror::Error;
use tokio::sync::Semaphore;
use url::Url;

pub mod associations;
pub mod cache;
pub mod ext;

#[cfg(all(test, feature = "reqwest"))]
mod tests;

pub mod builtins {
    use serde_json::Value;
    use std::sync::Arc;
    use url::Url;

    pub const TAPLO_CONFIG_URL: &str = "taplo://taplo.toml";

    #[must_use]
    pub fn taplo_config_schema() -> Arc<Value> {
        Arc::new(serde_json::to_value(schemars::schema_for!(crate::config::Config)).unwrap())
    }

    #[must_use]
    pub fn builtin_schema(url: &Url) -> Option<Arc<Value>> {
        if url.as_str() == TAPLO_CONFIG_URL {
            Some(taplo_config_schema())
        } else {
            None
        }
    }
}

/// `$ref`, `allOf`, `oneOf`, `anyOf` and the conditional applicators can point
/// back at the schema that contains them. Such a cycle makes no progress
/// against the traversal depth, which only counts property nesting, so
/// composition gets its own budget. Both traversals spend it, and both reset
/// it wherever a descent consumes a path segment.
const MAX_COMPOSITION_DEPTH: usize = 32;

#[derive(Clone)]
pub struct Schemas<E: Environment> {
    env: E,
    associations: SchemaAssociations<E>,
    concurrent_requests: Arc<Semaphore>,
    http: reqwest::Client,
    validators: Arc<Mutex<LruCache<Url, Arc<JSONSchema>>>>,
    cache: Cache<E>,
}

impl<E: Environment> Schemas<E> {
    pub fn new(env: E, http: reqwest::Client) -> Self {
        let cache = Cache::new(env.clone());

        Self {
            associations: SchemaAssociations::new(env.clone(), cache.clone(), http.clone()),
            cache,
            env,
            concurrent_requests: Arc::new(Semaphore::new(10)),
            http,
            validators: Arc::new(Mutex::new(LruCache::with_hasher(
                NonZeroUsize::new(3).unwrap(),
                ahash::RandomState::new(),
            ))),
        }
    }

    /// Get a reference to the schemas's associations.
    pub fn associations(&self) -> &SchemaAssociations<E> {
        &self.associations
    }

    /// Get a reference to the schemas's cache.
    pub fn cache(&self) -> &Cache<E> {
        &self.cache
    }

    pub fn env(&self) -> &E {
        &self.env
    }
}

impl<E: Environment> Schemas<E> {
    #[tracing::instrument(skip_all, fields(%schema_url))]
    pub async fn validate_root(
        &self,
        schema_url: &Url,
        root: &dom::Node,
    ) -> Result<Vec<NodeValidationError>, anyhow::Error> {
        let value = serde_json::to_value(root)?;
        self.validate(schema_url, &value)
            .await?
            .into_iter()
            .map(|error| NodeValidationError::new(root, error))
            .collect::<Result<Vec<_>, _>>()
    }

    #[tracing::instrument(skip_all, fields(%schema_url))]
    pub async fn validate(
        &self,
        schema_url: &Url,
        value: &Value,
    ) -> Result<Vec<ValidationError<'static>>, anyhow::Error> {
        let validator = match self.get_validator(schema_url) {
            Some(s) => s,
            None => {
                let schema = self
                    .load_schema(schema_url)
                    .await
                    .with_context(|| format!("failed to load schema {schema_url}"))?;
                self.add_schema(schema_url, schema.clone()).await;
                self.add_validator(schema_url.clone(), &schema)
                    .with_context(|| format!("invalid schema {schema_url}"))?
            }
        };

        self.validate_impl(&validator, value).await
    }

    async fn validate_impl(
        &self,
        validator: &JSONSchema,
        value: &Value,
    ) -> Result<Vec<ValidationError<'static>>, anyhow::Error> {
        // The following loop is required for retrieving external schemas.
        //
        // We don't know if any external schemas are required until we reach
        // a validation path that requires it, so we might have to loop many times
        // to fully validate according to a schema that has many nested references.
        loop {
            match validator.validate(value) {
                Ok(()) => return Ok(Vec::new()),
                Err(errors) => {
                    let errors: Vec<_> = errors
                        .map(|err| ValidationError {
                            instance: Cow::Owned(err.instance.into_owned()),
                            kind: err.kind,
                            instance_path: err.instance_path,
                            schema_path: err.schema_path,
                        })
                        .collect();

                    // We check whether there were any external schema errors,
                    // and retrieve the schemas accordingly.
                    let mut external_schema_requests: FuturesUnordered<_> = errors
                        .iter()
                        .filter_map(|err| {
                            if let ValidationErrorKind::Resolver { url, .. } = &err.kind {
                                Some(async {
                                    let value = self.load_schema(url).await?;
                                    drop(self.cache.store(url.clone(), value));
                                    Result::<(), anyhow::Error>::Ok(())
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    // There are no external schemas to retrieve,
                    // return the errors as-is.
                    if external_schema_requests.is_empty() {
                        drop(external_schema_requests);
                        return Ok(errors);
                    }

                    // Retrieve external schemas, and return on the first failure.
                    while let Some(external_schema_result) = external_schema_requests.next().await {
                        external_schema_result?;
                    }

                    // Try validation again, now with external schemas
                    // resolved and cached.
                    continue;
                }
            };
        }
    }

    pub async fn add_schema(&self, schema_url: &Url, schema: Arc<Value>) {
        drop(self.cache.store(schema_url.clone(), schema).await);
    }

    #[tracing::instrument(skip_all, fields(%schema_url))]
    pub async fn load_schema(&self, schema_url: &Url) -> Result<Arc<Value>, anyhow::Error> {
        if let Ok(s) = self.cache.load(schema_url, false).await {
            tracing::debug!(%schema_url, "schema was found in cache");
            return Ok(s);
        }

        let schema = if let Some(builtin) = builtin_schema(schema_url) {
            builtin
        } else {
            match self.fetch_external(schema_url).await {
                Ok(s) => Arc::new(s),
                Err(error) => {
                    tracing::warn!(%error, "failed to fetch schema");
                    if let Ok(s) = self.cache.load(schema_url, true).await {
                        tracing::debug!(%schema_url, "expired schema was found in cache");
                        return Ok(s);
                    }
                    return Err(error);
                }
            }
        };

        if let Err(error) = self.cache.store(schema_url.clone(), schema.clone()).await {
            tracing::debug!(%error, "failed to cache schema");
        }

        Ok(schema)
    }

    fn get_validator(&self, schema_url: &Url) -> Option<Arc<JSONSchema>> {
        if self.cache().lru_expired() {
            self.validators.lock().clear();
        }

        self.validators.lock().get(schema_url).cloned()
    }

    fn add_validator(
        &self,
        schema_url: Url,
        schema: &Value,
    ) -> Result<Arc<JSONSchema>, anyhow::Error> {
        let v = Arc::new(self.create_validator(schema)?);
        self.validators.lock().put(schema_url, v.clone());
        Ok(v)
    }

    #[async_recursion(?Send)]
    #[must_use]
    pub(crate) async fn resolve_schema(&self, url: Url) -> Result<Arc<Value>, anyhow::Error> {
        match url.fragment() {
            Some(fragment) => {
                let mut res_url = url.clone();
                res_url.set_fragment(None);
                let schema = self.resolve_schema(res_url).await?;
                let ptr = String::from("/") + fragment;
                schema
                    .pointer(&ptr)
                    .map(|v| Arc::new(v.clone()))
                    .ok_or_else(|| anyhow!("failed to resolve relative schema"))
            }
            None => {
                let val = self.load_schema(&url).await?;
                drop(self.cache.store(url, val.clone()));
                Ok(val)
            }
        }
    }

    fn create_validator(&self, schema: &Value) -> Result<JSONSchema, anyhow::Error> {
        let mut options = JSONSchema::options();

        options
            .with_resolver(CacheSchemaResolver {
                cache: self.cache().clone(),
            })
            .with_format("semver", formats::semver)
            .with_format("semver-requirement", formats::semver_req)
            // `format` is an annotation from 2019-09 on, so setting one of
            // those drafts would otherwise stop every format from asserting,
            // including the two registered above.
            .should_validate_formats(true);

        match declared_draft(schema) {
            // An explicit draft takes precedence over the `$schema` sniffing
            // in `jsonschema`, which matches meta-schema URIs exactly and so
            // misses the fragment-less form that 2019-09 and 2020-12 use.
            DeclaredDraft::Supported(draft) => {
                options.with_draft(draft);
            }
            DeclaredDraft::Unsupported(declared) => {
                tracing::warn!(
                    %declared,
                    used = "draft-07",
                    "schema declares a draft taplo cannot validate, validating as draft-07 instead"
                );
            }
            DeclaredDraft::Unrecognized => {}
        }

        options
            .compile(schema)
            .map_err(|err| anyhow!("invalid schema: {err}"))
    }

    async fn fetch_external(&self, schema_url: &Url) -> Result<Value, anyhow::Error> {
        let _permit = self.concurrent_requests.acquire().await?;
        match schema_url.scheme() {
            "http" | "https" => Ok(self
                .http
                .get(schema_url.clone())
                .send()
                .await?
                .json()
                .await?),
            "file" => Ok(serde_json::from_slice(
                &self
                    .env
                    .read_file(
                        self.env
                            .to_file_path_normalized(schema_url)
                            .ok_or_else(|| anyhow!("invalid file path"))?
                            .as_ref(),
                    )
                    .await?,
            )?),
            scheme => Err(anyhow!("the scheme `{scheme}` is not supported")),
        }
    }
}

impl<E: Environment> Schemas<E> {
    #[tracing::instrument(skip_all, fields(%schema_url, %path))]
    pub async fn schemas_at_path(
        &self,
        schema_url: &Url,
        value: &Value,
        path: &Keys,
    ) -> Result<Vec<(Keys, Arc<Value>)>, anyhow::Error> {
        let mut schemas = Vec::new();
        let schema = self.load_schema(schema_url).await?;
        self.collect_schemas(
            schema_url,
            &schema,
            value,
            Keys::empty(),
            path,
            MAX_COMPOSITION_DEPTH,
            &mut schemas,
        )
        .await?;

        schemas = schemas
            .into_iter()
            .unique_by(|(k, s)| (k.clone(), ArcHashValue(s.clone())))
            .collect();

        Ok(schemas)
    }

    /// Whether the instance satisfies a condition, or `None` when the
    /// condition cannot be decided and both branches have to be offered.
    ///
    /// A condition that is itself a reference is resolved, one hop, the way
    /// traversal resolves any schema carrying `$ref`. The condition compiles
    /// through `create_validator`, which sees no `$schema` on a subschema and
    /// so compiles it as draft 7 whatever the root declares — the floor a
    /// draft-4 root needs, since draft 4 has no `const` to discriminate on.
    async fn condition_holds(
        &self,
        root_url: &Url,
        condition: &Value,
        instance: &Value,
    ) -> Option<bool> {
        if instance.is_null() {
            return None;
        }

        let resolved = self.ref_schema_value(root_url, condition).await;
        let condition = resolved.as_deref().unwrap_or(condition);

        if names_a_ref(condition) {
            return None;
        }

        match self.create_validator(condition) {
            Ok(validator) => Some(validator.is_valid(instance)),
            Err(error) => {
                tracing::debug!(%error, "condition could not be compiled");
                None
            }
        }
    }

    /// The subschemas that apply to the same instance as `schema` itself and
    /// consume no path: the `if` branch the instance selects.
    ///
    /// An absent instance, or a condition that cannot be decided, yields both
    /// branches, which is what traversal already offers for `oneOf` and
    /// `anyOf`.
    async fn conditional_subschemas<'s>(
        &self,
        root_url: &Url,
        schema: &'s Value,
        instance: &Value,
    ) -> Vec<&'s Value> {
        let mut applicable = Vec::new();

        let branches = [&schema["then"], &schema["else"]];

        if !schema["if"].is_null() && branches.iter().any(|branch| !branch.is_null()) {
            let selected: &[&Value] = match self
                .condition_holds(root_url, &schema["if"], instance)
                .await
            {
                Some(true) => &branches[..1],
                Some(false) => &branches[1..],
                None => &branches,
            };

            applicable.extend(selected.iter().copied().filter(|b| !b.is_null()));
        }

        applicable
    }

    #[tracing::instrument(skip_all, fields(%path))]
    #[async_recursion(?Send)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    async fn collect_schemas(
        &self,
        root_url: &Url,
        schema: &Value,
        value: &Value,
        full_path: Keys,
        path: &Keys,
        composition_depth: usize,
        schemas: &mut Vec<(Keys, Arc<Value>)>,
    ) -> Result<(), anyhow::Error> {
        if !schema.is_object() || composition_depth == 0 {
            return Ok(());
        }

        let composition_depth = composition_depth - 1;

        if let Some(r) = schema.schema_ref() {
            let url = reference_url(root_url, r)
                .ok_or_else(|| anyhow!("could not determine schema URL"))?;
            let schema = self.resolve_schema(url).await?;
            return self
                .collect_schemas(
                    root_url,
                    &schema,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    schemas,
                )
                .await;
        }

        if let Some(one_ofs) = schema["oneOf"].as_array() {
            for one_of in one_ofs {
                self.collect_schemas(
                    root_url,
                    one_of,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    schemas,
                )
                .await?;
            }
        }

        if let Some(any_ofs) = schema["anyOf"].as_array() {
            for any_of in any_ofs {
                self.collect_schemas(
                    root_url,
                    any_of,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    schemas,
                )
                .await?;
            }
        }

        if let Some(all_ofs) = schema["allOf"].as_array() {
            for all_of in all_ofs {
                self.collect_schemas(
                    root_url,
                    all_of,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    schemas,
                )
                .await?;
            }
        }

        for conditional in self.conditional_subschemas(root_url, schema, value).await {
            self.collect_schemas(
                root_url,
                conditional,
                value,
                full_path.clone(),
                path,
                composition_depth,
                schemas,
            )
            .await?;
        }

        let include_self = schema["allOf"].is_null();

        let Some(key) = path.iter().next() else {
            if include_self {
                schemas.push((full_path.clone(), Arc::new(schema.clone())));
            }
            return Ok(());
        };

        let child_path = path.skip_left(1);

        match key {
            KeyOrIndex::Key(k) => {
                // For array of tables.
                self.collect_schemas(
                    root_url,
                    &schema["items"][k.value()],
                    &value[k.value()],
                    full_path.join(k.clone()),
                    &child_path,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await?;

                self.collect_schemas(
                    root_url,
                    &schema["properties"][k.value()],
                    &value[k.value()],
                    full_path.join(k.clone()),
                    &child_path,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await?;

                self.collect_schemas(
                    root_url,
                    &schema["additionalProperties"],
                    &value[k.value()],
                    full_path.join(k.clone()),
                    &child_path,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await?;

                if let Some(pattern_props) = schema["patternProperties"].as_object() {
                    for (pattern, pattern_schema) in pattern_props {
                        if let Ok(re) = Regex::new(pattern) {
                            if re.is_match(k.value()) {
                                self.collect_schemas(
                                    root_url,
                                    pattern_schema,
                                    &value[k.value()],
                                    full_path.join(k.clone()),
                                    &child_path,
                                    MAX_COMPOSITION_DEPTH,
                                    schemas,
                                )
                                .await?;
                            }
                        }
                    }
                }
            }
            KeyOrIndex::Index(idx) => {
                // `prefixItems` and `items` partition the array: the leading
                // positions belong to `prefixItems`, the rest to `items`.
                let covered_by_prefix_items = schema["prefixItems"]
                    .as_array()
                    .is_some_and(|prefix_items| *idx < prefix_items.len());

                let item_schema = if covered_by_prefix_items {
                    &schema["prefixItems"][idx]
                } else if schema["items"].is_array() {
                    &schema["items"][idx]
                } else {
                    &schema["items"]
                };

                self.collect_schemas(
                    root_url,
                    item_schema,
                    &value[idx],
                    full_path.join(*idx),
                    &child_path,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await?;
            }
        }

        Ok(())
    }

    #[tracing::instrument(skip_all, fields(%schema_url, %path))]
    pub async fn possible_schemas_from(
        &self,
        schema_url: &Url,
        value: &Value,
        path: &Keys,
        max_depth: usize,
    ) -> Result<Vec<(Keys, Keys, Arc<Value>)>, anyhow::Error> {
        let schemas = self.schemas_at_path(schema_url, value, path).await?;

        let mut children = Vec::with_capacity(schemas.len());

        for (path, schema) in schemas {
            self.collect_child_schemas(
                schema_url,
                &schema,
                &path,
                &Keys::empty(),
                instance_at(value, &path),
                max_depth,
                MAX_COMPOSITION_DEPTH,
                &mut children,
            )
            .await;
        }

        children = children
            .into_iter()
            .unique_by(|(k1, k2, s)| (k1.clone(), k2.clone(), ArcHashValue(s.clone())))
            .collect();

        Ok(children)
    }

    #[async_recursion(?Send)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    async fn collect_child_schemas(
        &self,
        root_url: &Url,
        schema: &Value,
        root_path: &Keys,
        path: &Keys,
        instance: &Value,
        mut depth: usize,
        composition_depth: usize,
        schemas: &mut Vec<(Keys, Keys, Arc<Value>)>,
    ) {
        if !schema.is_object() || depth == 0 || composition_depth == 0 {
            return;
        }

        let composition_depth = composition_depth - 1;

        if let Some(schema) = self.ref_schema_value(root_url, schema).await {
            return self
                .collect_child_schemas(
                    root_url,
                    &schema,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    schemas,
                )
                .await;
        }

        if let Some(one_ofs) = schema["oneOf"].as_array() {
            for one_of in one_ofs {
                self.collect_child_schemas(
                    root_url,
                    one_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    schemas,
                )
                .await;
            }
        }

        if let Some(any_ofs) = schema["anyOf"].as_array() {
            for any_of in any_ofs {
                self.collect_child_schemas(
                    root_url,
                    any_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    schemas,
                )
                .await;
            }
        }

        for conditional in self
            .conditional_subschemas(root_url, schema, instance)
            .await
        {
            self.collect_child_schemas(
                root_url,
                conditional,
                root_path,
                path,
                instance,
                depth,
                composition_depth,
                schemas,
            )
            .await;
        }

        // Deal with the { "description": "Foo", "allOf": [{ "$ref": "Bar" }] }
        // pattern.
        let composed = [
            !schema["allOf"].is_null(),
            !schema["oneOf"].is_null(),
            !schema["anyOf"].is_null(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count()
            == 1
            && schema["properties"].is_null();

        if let Some(all_ofs) = schema["allOf"].as_array() {
            if !all_ofs.is_empty() && composed {
                let mut schema = schema.clone();
                if let Some(obj) = schema.as_object_mut() {
                    obj.remove("allOf");
                }

                let mut merged_all_of = Value::Object(serde_json::Map::default());

                for all_of in all_ofs {
                    merged_all_of.merge(match self.ref_schema_value(root_url, all_of).await {
                        Some(ref schema) => schema,
                        None => all_of,
                    });
                }

                merged_all_of.merge(&schema);

                self.collect_child_schemas(
                    root_url,
                    &merged_all_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    schemas,
                )
                .await;
            }
            // TODO: handle allOfs in regular schemas.
        }

        let include_self = !composed;

        if include_self {
            schemas.push((
                root_path.extend(path.clone()),
                path.clone(),
                Arc::new(schema.clone()),
            ));
        }

        depth -= 1;

        if let Some(map) = schema["properties"].as_object() {
            for (k, v) in map {
                self.collect_child_schemas(
                    root_url,
                    v,
                    root_path,
                    &path.join(Key::from(k)),
                    &instance[k],
                    depth,
                    MAX_COMPOSITION_DEPTH,
                    schemas,
                )
                .await;
            }
        }
    }

    async fn ref_schema_value(&self, root_url: &Url, schema: &Value) -> Option<Arc<Value>> {
        if let Some(r) = schema.schema_ref() {
            let url = match reference_url(root_url, r)
                .ok_or_else(|| anyhow!("could not determine schema URL"))
            {
                Ok(u) => u,
                Err(error) => {
                    tracing::error!(?error, "failed to resolve schema");
                    return None;
                }
            };
            let schema = match self.resolve_schema(url).await {
                Ok(s) => s,
                Err(error) => {
                    tracing::error!(?error, "failed to resolve schema");
                    return None;
                }
            };

            Some(schema)
        } else {
            None
        }
    }
}

/// The part of the document a schema at `keys` applies to.
///
/// Indexing a `Value` with a missing key or an out-of-range index yields
/// `Value::Null`, which is how an absent position reports itself.
fn instance_at<'v>(value: &'v Value, keys: &Keys) -> &'v Value {
    let mut instance = value;

    for key in keys.iter() {
        instance = match key {
            KeyOrIndex::Key(k) => &instance[k.value()],
            KeyOrIndex::Index(idx) => &instance[*idx],
        };
    }

    instance
}

/// Whether a subschema names a reference anywhere within it.
///
/// A subschema compiled on its own keeps its `#/...` pointers and loses the
/// document they point into, so every reference fails to resolve and the
/// subschema rejects every instance. A condition that names one is therefore
/// undecidable rather than false.
fn names_a_ref(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => {
            map.get("$ref").is_some_and(Value::is_string) || map.values().any(names_a_ref)
        }
        Value::Array(items) => items.iter().any(names_a_ref),
        _ => false,
    }
}

fn reference_url(root_url: &Url, reference: &str) -> Option<Url> {
    if !reference.starts_with('#') {
        return Url::parse(reference).ok();
    }
    let mut url = root_url.clone();
    url.set_fragment(Some(reference.trim_start_matches("#/")));
    Some(url)
}

/// How a schema's `$schema` value maps onto a draft taplo can validate against.
#[derive(Debug, PartialEq, Eq)]
enum DeclaredDraft {
    Supported(Draft),
    /// A meta-schema taplo recognizes as one but has no validator for. Carries
    /// the declared URI so the warning can name it.
    Unsupported(String),
    Unrecognized,
}

/// Classifies the root `$schema`, which is the only one that counts: a
/// compiled `JSONSchema` carries a single draft, and every document reached
/// through `$ref` is compiled under it.
fn declared_draft(schema: &Value) -> DeclaredDraft {
    let Some(declared) = schema["$schema"].as_str() else {
        return DeclaredDraft::Unrecognized;
    };

    // Classification goes by host and path, so scheme, case, query and
    // fragment do not matter. Stripping the trailing `#` that the canonical
    // form carries up to draft-07 only keeps the reported URI tidy.
    let normalized = declared.strip_suffix('#').unwrap_or(declared);

    let Ok(url) = Url::parse(normalized) else {
        return DeclaredDraft::Unrecognized;
    };

    if url.host_str() != Some("json-schema.org") {
        return DeclaredDraft::Unrecognized;
    }

    match url.path() {
        "/draft-04/schema" => DeclaredDraft::Supported(Draft::Draft4),
        "/draft-06/schema" => DeclaredDraft::Supported(Draft::Draft6),
        "/draft-07/schema" => DeclaredDraft::Supported(Draft::Draft7),
        "/draft/2019-09/schema" => DeclaredDraft::Supported(Draft::Draft201909),
        "/draft/2020-12/schema" => DeclaredDraft::Supported(Draft::Draft202012),
        _ => DeclaredDraft::Unsupported(normalized.to_owned()),
    }
}

pub trait ValueExt {
    fn is_schema_ref(&self) -> bool;
    fn schema_ref(&self) -> Option<&str>;
}

impl ValueExt for Value {
    fn is_schema_ref(&self) -> bool {
        self["$ref"].is_string()
    }

    fn schema_ref(&self) -> Option<&str> {
        self["$ref"].as_str()
    }
}

struct CacheSchemaResolver<E: Environment> {
    cache: Cache<E>,
}

impl<E: Environment> SchemaResolver for CacheSchemaResolver<E> {
    fn resolve(
        &self,
        _root_schema: &serde_json::Value,
        url: &Url,
        _original_ref: &str,
    ) -> Result<Arc<serde_json::Value>, jsonschema::SchemaResolverError> {
        self.cache
            .get_schema(url)
            .ok_or_else(|| WouldBlockError.into())
    }
}

#[derive(Debug, Error)]
#[error("retrieving the schema requires external operations")]
struct WouldBlockError;

/// A validation error that contains text ranges as well.
#[derive(Debug)]
pub struct NodeValidationError {
    pub keys: Keys,
    pub node: dom::Node,
    pub error: ValidationError<'static>,
}

impl NodeValidationError {
    fn new(root: &dom::Node, error: ValidationError<'static>) -> Result<Self, anyhow::Error> {
        let mut keys = Keys::empty();
        let mut node = root.clone();

        match &error.kind {
            ValidationErrorKind::AdditionalProperties { unexpected } => {
                keys = keys.extend(unexpected.iter().map(Key::from).map(KeyOrIndex::Key));
            }
            _ => {}
        }

        'outer: for path in &error.instance_path {
            match path {
                jsonschema::paths::PathChunk::Property(p) => match node {
                    dom::Node::Table(t) => {
                        let entries = t.entries().read();
                        for (k, entry) in entries.iter() {
                            if k.value() == &**p {
                                keys = keys.join(k.clone());
                                node = entry.clone();
                                continue 'outer;
                            }
                        }
                        return Err(anyhow!("invalid key"));
                    }
                    _ => return Err(anyhow!("invalid key")),
                },
                jsonschema::paths::PathChunk::Index(idx) => {
                    node = node.try_get(*idx).map_err(|_| anyhow!("invalid index"))?;
                    keys = keys.join(*idx);
                }
                jsonschema::paths::PathChunk::Keyword(_) => {}
            }
        }

        Ok(Self { keys, node, error })
    }

    #[must_use]
    pub fn text_ranges(&self) -> Box<dyn Iterator<Item = TextRange> + '_> {
        match self.error.kind {
            ValidationErrorKind::AdditionalProperties { .. } => {
                let include_children = false;

                if self.keys.is_empty() {
                    return Box::new(self.node.text_ranges(include_children));
                }

                Box::new(
                    self.keys
                        .clone()
                        .into_iter()
                        .flat_map(move |key| self.node.get(key).text_ranges(include_children)),
                )
            }
            _ => Box::new(self.node.text_ranges(true)),
        }
    }
}

mod formats {
    pub(super) fn semver(value: &str) -> bool {
        semver::Version::parse(value).is_ok()
    }

    pub(super) fn semver_req(value: &str) -> bool {
        semver::VersionReq::parse(value).is_ok()
    }
}
