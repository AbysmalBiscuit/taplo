use self::{associations::SchemaAssociations, builtins::builtin_schema, cache::Cache};
use crate::{environment::Environment, util::ArcHashValue, LruCache};
use anyhow::{anyhow, Context};
use async_recursion::async_recursion;
use itertools::Itertools;
use jsonschema::{error::ValidationErrorKind, Draft, Retrieve, Uri, ValidationError, Validator};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value;
use std::{collections::HashMap, num::NonZeroUsize, sync::Arc};
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
    validators: Arc<Mutex<LruCache<Url, Arc<Validator>>>>,
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
                    .await
                    .with_context(|| format!("invalid schema {schema_url}"))?
            }
        };

        Ok(validator
            .iter_errors(value)
            .map(ValidationError::to_owned)
            .collect())
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

    fn get_validator(&self, schema_url: &Url) -> Option<Arc<Validator>> {
        if self.cache().lru_expired() {
            self.validators.lock().clear();
        }

        self.validators.lock().get(schema_url).cloned()
    }

    async fn add_validator(
        &self,
        schema_url: Url,
        schema: &Value,
    ) -> Result<Arc<Validator>, anyhow::Error> {
        let v = Arc::new(self.create_validator(&schema_url, schema).await?);
        self.validators.lock().put(schema_url, v.clone());
        Ok(v)
    }

    /// The schema at `url`, with the base in force *around* it.
    ///
    /// A JSON pointer may cross objects that carry `$id`, each of which
    /// re-bases what lies beneath it, so the base is the document's URL joined
    /// with every `$id` the pointer walked *through*. The target's own `$id` is
    /// deliberately not applied: every consumer re-bases on entry, and applying
    /// it here as well would join it twice — `{"$id": "defs/"}` would resolve a
    /// sibling reference against `defs/defs/`.
    #[must_use]
    pub(crate) async fn resolve_schema(
        &self,
        url: Url,
    ) -> Result<(Url, Arc<Value>), anyhow::Error> {
        let mut document_url = url.clone();
        document_url.set_fragment(None);

        let (document_base, document) = match self.cache.resource_at(&document_url) {
            Some(resource) => resource,
            None => (document_url.clone(), self.load_schema(&document_url).await?),
        };

        let fragment = url.fragment().unwrap_or_default();
        if fragment.is_empty() {
            return Ok((document_base, document));
        }

        // A URI fragment is percent-encoded where a JSON pointer is not, so it
        // is decoded before use. `~0` and `~1` survive that untouched and are
        // unescaped per token below.
        let pointer = percent_encoding::percent_decode_str(fragment)
            .decode_utf8()
            .with_context(|| format!("reference fragment is not valid UTF-8: {fragment}"))?;

        if !pointer.starts_with('/') {
            return anchored_subschema(&document, &document_base, &url)
                .map(|(anchor_base, schema)| (anchor_base, Arc::new(schema.clone())))
                .ok_or_else(|| anyhow!("failed to resolve reference `{url}`"));
        }

        let mut base = document_base;
        let mut target = &*document;

        for token in pointer.split('/').skip(1) {
            // The `$id` of an object the pointer passes *through* re-bases what
            // lies below it. The one on the object the pointer lands on is left
            // to the traversal, which re-bases on entry.
            if let Some(rebased) = rebase(&base, target) {
                base = rebased;
            }

            let token = token.replace("~1", "/").replace("~0", "~");

            target = match target {
                Value::Object(map) => map.get(&token),
                Value::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get(i)),
                _ => None,
            }
            .ok_or_else(|| anyhow!("failed to resolve reference `{url}`"))?;
        }

        Ok((base, Arc::new(target.clone())))
    }

    async fn create_validator(
        &self,
        base_url: &Url,
        schema: &Value,
    ) -> Result<Validator, anyhow::Error> {
        let schema = absolute_refs(schema, base_url);
        let pending = Arc::new(Mutex::new(Vec::new()));
        let documents = Arc::new(Mutex::new(HashMap::new()));
        let mut options = jsonschema::options()
            .with_base_uri(base_url.as_str())
            .with_retriever(CacheSchemaRetriever {
                cache: self.cache().clone(),
                pending: pending.clone(),
                documents: documents.clone(),
            })
            .with_format("semver", formats::semver)
            .with_format("semver-requirement", formats::semver_req)
            .should_validate_formats(true);

        let draft = match declared_draft(&schema) {
            DeclaredDraft::Supported(draft) => draft,
            DeclaredDraft::Unsupported(declared) => {
                tracing::warn!(
                    %declared,
                    used = "draft-07",
                    "schema declares a draft taplo cannot validate, validating as draft-07 instead"
                );
                Draft::Draft7
            }
            DeclaredDraft::Unrecognized => Draft::Draft7,
        };
        options = options.with_draft(draft);

        loop {
            match options.build(&schema) {
                Ok(validator) => return Ok(validator),
                Err(error) => {
                    let requests = std::mem::take(&mut *pending.lock());
                    if requests.is_empty() {
                        return Err(anyhow!("invalid schema: {error}"));
                    }
                    for url in requests {
                        let document = self
                            .load_schema(&url)
                            .await
                            .with_context(|| format!("failed to load referenced schema {url}"))?;
                        documents.lock().insert(url, document);
                    }
                }
            }
        }
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
        let _evaluated = self
            .collect_schemas(
                schema_url,
                &schema,
                value,
                Keys::empty(),
                path,
                MAX_COMPOSITION_DEPTH,
                &mut Vec::new(),
                &mut schemas,
            )
            .await?;

        schemas = schemas
            .into_iter()
            .unique_by(|(k, s)| (k.clone(), ArcHashValue(s.clone())))
            .collect();

        Ok(schemas)
    }

    /// Whether the instance satisfies a condition, or `None` when the condition
    /// cannot be decided and both branches have to be offered.
    async fn condition_holds(
        &self,
        base_url: &Url,
        condition: &Value,
        instance: &Value,
    ) -> Option<bool> {
        if instance.is_null() {
            return None;
        }

        let mut condition = absolute_refs(condition, base_url);
        if let Some(object) = condition.as_object_mut() {
            object.remove("$id");
            if let Some(root) = self.cache.get_schema(base_url) {
                if matches!(
                    declared_draft(&root),
                    DeclaredDraft::Supported(Draft::Draft201909 | Draft::Draft202012)
                ) {
                    object
                        .entry("$schema")
                        .or_insert_with(|| root["$schema"].clone());
                }
            }
        }
        // A detached condition must not shadow the document its pointers name.
        let condition_url = Url::parse("taplo://condition").unwrap();
        match self.create_validator(&condition_url, &condition).await {
            Ok(validator) => Some(validator.is_valid(instance)),
            Err(error) => {
                tracing::debug!(%error, "condition could not be compiled");
                None
            }
        }
    }

    /// The subschemas that apply to the same instance as `schema` itself and
    /// consume no path: the `if` branch the instance selects, and the schemas
    /// its present keys depend on.
    ///
    /// An absent instance, or a condition that cannot be decided, yields every
    /// branch and every dependent schema, which is what traversal already
    /// offers for `oneOf` and `anyOf`.
    async fn conditional_subschemas<'s>(
        &self,
        base_url: &Url,
        schema: &'s Value,
        instance: &Value,
    ) -> Vec<&'s Value> {
        let mut applicable = Vec::new();

        let branches = [&schema["then"], &schema["else"]];

        if !schema["if"].is_null() && branches.iter().any(|branch| !branch.is_null()) {
            let selected: &[&Value] = match self
                .condition_holds(base_url, &schema["if"], instance)
                .await
            {
                Some(true) => &branches[..1],
                Some(false) => &branches[1..],
                None => &branches,
            };

            applicable.extend(selected.iter().copied().filter(|b| !b.is_null()));
        }

        // Array dependencies name required keys rather than subschemas.
        for keyword in ["dependencies", "dependentSchemas"] {
            let Some(dependents) = schema[keyword].as_object() else {
                continue;
            };

            for (trigger, dependent) in dependents {
                if !dependent.is_object() {
                    continue;
                }

                if instance.is_null() || instance.get(trigger).is_some() {
                    applicable.push(dependent);
                }
            }
        }

        applicable
    }

    #[tracing::instrument(skip_all, fields(%path))]
    #[async_recursion(?Send)]
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    async fn collect_schemas(
        &self,
        base_url: &Url,
        schema: &Value,
        value: &Value,
        full_path: Keys,
        path: &Keys,
        composition_depth: usize,
        visited: &mut Vec<Url>,
        schemas: &mut Vec<(Keys, Arc<Value>)>,
    ) -> Result<bool, anyhow::Error> {
        if !schema.is_object() || composition_depth == 0 {
            return Ok(false);
        }

        let composition_depth = composition_depth - 1;

        let enclosing_base = base_url;
        let rebased = rebase(base_url, schema);
        let base_url = rebased.as_ref().unwrap_or(base_url);

        if let Some(r) = schema.schema_ref() {
            let url = reference_url(base_url, r)
                .ok_or_else(|| anyhow!("could not determine schema URL"))?;

            // A reference already followed on this chain leads back to a schema
            // whose contribution is already in the accumulator, so following it
            // again only multiplies the work a cycle costs.
            if visited.contains(&url) {
                return Ok(false);
            }

            let (target_base, target) = self.resolve_schema(url.clone()).await?;
            let merged = self.fold_siblings(target, schema, base_url);

            visited.push(url);
            let evaluated = self
                .collect_schemas(
                    &target_base,
                    &merged,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            visited.pop();

            return evaluated;
        }

        let composed = self
            .compose_all_of(enclosing_base, schema, composition_depth, visited)
            .await;
        let schema = composed.as_ref().unwrap_or(schema);

        let mut evaluated = false;

        for keyword in ["oneOf", "anyOf"] {
            let Some(branches) = schema[keyword].as_array() else {
                continue;
            };
            let mut coverage = Vec::with_capacity(branches.len());
            for branch in branches {
                let covers = self
                    .collect_schemas(
                        base_url,
                        branch,
                        value,
                        full_path.clone(),
                        path,
                        composition_depth,
                        visited,
                        schemas,
                    )
                    .await?;
                coverage.push(covers);
            }
            if coverage.iter().any(|covers| *covers) {
                let mut matching = 0;
                let mut branch_evaluated = false;
                for (branch, covers) in branches.iter().zip(coverage) {
                    if self.condition_holds(base_url, branch, value).await != Some(false) {
                        matching += 1;
                        branch_evaluated |= covers;
                    }
                }
                if keyword == "anyOf" || matching == 1 {
                    evaluated |= branch_evaluated;
                }
            }
        }

        if let Some(all_ofs) = schema["allOf"].as_array() {
            for all_of in all_ofs {
                evaluated |= self
                    .collect_schemas(
                        base_url,
                        all_of,
                        value,
                        full_path.clone(),
                        path,
                        composition_depth,
                        visited,
                        schemas,
                    )
                    .await?;
            }
        }

        for conditional in self.conditional_subschemas(base_url, schema, value).await {
            evaluated |= self
                .collect_schemas(
                    base_url,
                    conditional,
                    value,
                    full_path.clone(),
                    path,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await?;
        }

        let Some(key) = path.iter().next() else {
            schemas.push((
                full_path.clone(),
                Arc::new(absolute_refs(schema, enclosing_base)),
            ));
            return Ok(false);
        };

        let child_path = path.skip_left(1);

        match key {
            KeyOrIndex::Key(k) => {
                // For array of tables.
                let _ = self
                    .collect_schemas(
                        base_url,
                        &schema["items"][k.value()],
                        &value[k.value()],
                        full_path.join(k.clone()),
                        &child_path,
                        MAX_COMPOSITION_DEPTH,
                        &mut Vec::new(),
                        schemas,
                    )
                    .await?;

                let _ = self
                    .collect_schemas(
                        base_url,
                        &schema["properties"][k.value()],
                        &value[k.value()],
                        full_path.join(k.clone()),
                        &child_path,
                        MAX_COMPOSITION_DEPTH,
                        &mut Vec::new(),
                        schemas,
                    )
                    .await?;
                evaluated |= !schema["properties"][k.value()].is_null();

                let _ = self
                    .collect_schemas(
                        base_url,
                        &schema["additionalProperties"],
                        &value[k.value()],
                        full_path.join(k.clone()),
                        &child_path,
                        MAX_COMPOSITION_DEPTH,
                        &mut Vec::new(),
                        schemas,
                    )
                    .await?;
                evaluated |= !schema["additionalProperties"].is_null();

                if let Some(pattern_props) = schema["patternProperties"].as_object() {
                    for (pattern, pattern_schema) in pattern_props {
                        if let Ok(re) = Regex::new(pattern) {
                            if re.is_match(k.value()) {
                                let _ = self
                                    .collect_schemas(
                                        base_url,
                                        pattern_schema,
                                        &value[k.value()],
                                        full_path.join(k.clone()),
                                        &child_path,
                                        MAX_COMPOSITION_DEPTH,
                                        &mut Vec::new(),
                                        schemas,
                                    )
                                    .await?;
                                evaluated = true;
                            }
                        }
                    }
                }

                // `unevaluatedProperties` applies to a key no other applicator
                // in this schema evaluated, which is the question every
                // in-place recursion above has just answered for this key.
                if !evaluated {
                    let _ = self
                        .collect_schemas(
                            base_url,
                            &schema["unevaluatedProperties"],
                            &value[k.value()],
                            full_path.join(k.clone()),
                            &child_path,
                            MAX_COMPOSITION_DEPTH,
                            &mut Vec::new(),
                            schemas,
                        )
                        .await?;
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
                    schema["items"]
                        .get(*idx)
                        .unwrap_or(&schema["additionalItems"])
                } else {
                    &schema["items"]
                };

                let _ = self
                    .collect_schemas(
                        base_url,
                        item_schema,
                        &value[idx],
                        full_path.join(*idx),
                        &child_path,
                        MAX_COMPOSITION_DEPTH,
                        &mut Vec::new(),
                        schemas,
                    )
                    .await?;
                evaluated |= !item_schema.is_null();

                if !schema["contains"].is_null()
                    && self
                        .condition_holds(base_url, &schema["contains"], &value[idx])
                        .await
                        == Some(true)
                {
                    evaluated = true;
                }

                if !evaluated {
                    let _ = self
                        .collect_schemas(
                            base_url,
                            &schema["unevaluatedItems"],
                            &value[idx],
                            full_path.join(*idx),
                            &child_path,
                            MAX_COMPOSITION_DEPTH,
                            &mut Vec::new(),
                            schemas,
                        )
                        .await?;
                    evaluated |= !schema["unevaluatedItems"].is_null();
                }
            }
        }

        Ok(evaluated)
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
                &mut Vec::new(),
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
        base_url: &Url,
        schema: &Value,
        root_path: &Keys,
        path: &Keys,
        instance: &Value,
        mut depth: usize,
        composition_depth: usize,
        visited: &mut Vec<Url>,
        schemas: &mut Vec<(Keys, Keys, Arc<Value>)>,
    ) {
        if !schema.is_object() || depth == 0 || composition_depth == 0 {
            return;
        }

        let composition_depth = composition_depth - 1;

        let enclosing_base = base_url;
        let rebased = rebase(base_url, schema);
        let base_url = rebased.as_ref().unwrap_or(base_url);

        if let Some((url, target_base, resolved)) = self.ref_schema_value(base_url, schema).await {
            if visited.contains(&url) {
                return;
            }

            visited.push(url);
            self.collect_child_schemas(
                &target_base,
                &resolved,
                root_path,
                path,
                instance,
                depth,
                composition_depth,
                visited,
                schemas,
            )
            .await;
            visited.pop();

            return;
        }

        let composed = self
            .compose_all_of(enclosing_base, schema, composition_depth, visited)
            .await;
        let schema = composed.as_ref().unwrap_or(schema);

        if let Some(one_ofs) = schema["oneOf"].as_array() {
            for one_of in one_ofs {
                self.collect_child_schemas(
                    base_url,
                    one_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            }
        }

        if let Some(any_ofs) = schema["anyOf"].as_array() {
            for any_of in any_ofs {
                self.collect_child_schemas(
                    base_url,
                    any_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            }
        }

        for conditional in self
            .conditional_subschemas(base_url, schema, instance)
            .await
        {
            self.collect_child_schemas(
                base_url,
                conditional,
                root_path,
                path,
                instance,
                depth,
                composition_depth,
                visited,
                schemas,
            )
            .await;
        }

        if let Some(all_ofs) = schema["allOf"].as_array() {
            for all_of in all_ofs {
                self.collect_child_schemas(
                    base_url,
                    all_of,
                    root_path,
                    path,
                    instance,
                    depth,
                    composition_depth,
                    visited,
                    schemas,
                )
                .await;
            }
        }

        let include_self = schema["oneOf"].is_null() && schema["anyOf"].is_null()
            || !schema["properties"].is_null();

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
                    base_url,
                    v,
                    root_path,
                    &path.join(Key::from(k)),
                    &instance[k],
                    depth,
                    MAX_COMPOSITION_DEPTH,
                    &mut Vec::new(),
                    schemas,
                )
                .await;
            }
        }
    }

    #[async_recursion(?Send)]
    async fn compose_all_of(
        &self,
        base_url: &Url,
        schema: &Value,
        budget: usize,
        visited: &mut Vec<Url>,
    ) -> Option<Value> {
        let all_ofs = schema["allOf"].as_array()?;
        if budget == 0
            || !schema["oneOf"].is_null()
            || !schema["anyOf"].is_null()
            || !schema["properties"].is_null()
        {
            return None;
        }
        let enclosing_base = base_url;
        let rebased = rebase(base_url, schema);
        let base_url = rebased.as_ref().unwrap_or(base_url);
        let mut merged = Value::Object(Default::default());
        for member in all_ofs {
            let resolved = self.ref_schema_value(base_url, member).await;
            let (member_base, member) = match &resolved {
                Some((url, base, value)) => {
                    if visited.contains(url) {
                        continue;
                    }
                    visited.push(url.clone());
                    (base, &**value)
                }
                None => (base_url, member),
            };
            let composed = self
                .compose_all_of(member_base, member, budget - 1, visited)
                .await;
            let member = absolute_refs(composed.as_ref().unwrap_or(member), member_base);
            merged = merged_all_of(&merged, &member);
            if resolved.is_some() {
                visited.pop();
            }
        }
        let mut carrier = absolute_refs(schema, enclosing_base);
        carrier.as_object_mut()?.remove("allOf");
        let merged = merged_all_of(&merged, &carrier);
        // Array evaluation depends on applicator boundaries, which a merge would erase.
        if [
            "items",
            "prefixItems",
            "contains",
            "additionalItems",
            "unevaluatedItems",
        ]
        .iter()
        .any(|keyword| !merged[*keyword].is_null())
        {
            return None;
        }
        Some(merged)
    }

    /// The schema a `$ref` names, with the URL it resolved to and the base in
    /// force around it.
    ///
    /// The URL is what the visited set is keyed on, so it has to travel with
    /// the value rather than be recomputed by the caller.
    async fn ref_schema_value(
        &self,
        base_url: &Url,
        schema: &Value,
    ) -> Option<(Url, Url, Arc<Value>)> {
        let r = schema.schema_ref()?;

        let url = match reference_url(base_url, r) {
            Some(u) => u,
            None => {
                tracing::error!(reference = r, "could not determine schema URL");
                return None;
            }
        };

        let (target_base, target) = match self.resolve_schema(url.clone()).await {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::error!(?error, "failed to resolve schema");
                return None;
            }
        };

        Some((
            url,
            target_base,
            self.fold_siblings(target, schema, base_url),
        ))
    }

    /// `target` with the keywords written beside the `$ref` that named it
    /// merged over the top.
    ///
    /// The overlay's own references are written in the carrier's document, so
    /// they are made absolute against the carrier's base before the merge. The
    /// merged value is then traversed under the target's base, which is where
    /// everything the target itself wrote belongs.
    fn fold_siblings(&self, target: Arc<Value>, carrier: &Value, base_url: &Url) -> Arc<Value> {
        match sibling_overlay(carrier) {
            Some(overlay) => Arc::new(merged_over(&target, &absolute_refs(&overlay, base_url))),
            None => target,
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

/// The base a schema object establishes for everything written inside it.
///
/// `$id` is a URI reference like any other, resolved against the base in force
/// where it appears; its fragment names the object rather than a document, so
/// it is dropped from the base. Draft 4 spells this keyword `id`, which
/// traversal does not read: `id` stopped being a keyword in draft 6, and a
/// schema still carrying one as leftover metadata would otherwise re-base
/// every reference beneath it.
fn rebase(base: &Url, schema: &Value) -> Option<Url> {
    let id = schema["$id"].as_str()?;
    let mut url = base.join(id).ok()?;
    url.set_fragment(None);
    Some(url)
}

/// Find a resource or anchor, retaining the scope around the matching schema.
fn anchored_subschema<'d>(document: &'d Value, base: &Url, url: &Url) -> Option<(Url, &'d Value)> {
    let declared = document["$id"].as_str().and_then(|id| base.join(id).ok());
    if declared.as_ref() == Some(url) {
        return Some((base.clone(), document));
    }
    let scope = rebase(base, document).unwrap_or_else(|| base.clone());
    for keyword in ["$anchor", "$dynamicAnchor"] {
        if let Some(anchor) = document[keyword].as_str() {
            let mut anchored = scope.clone();
            anchored.set_fragment(Some(anchor));
            if &anchored == url {
                return Some((base.clone(), document));
            }
        }
    }
    schema_children(document).find_map(|child| anchored_subschema(child, &scope, url))
}

fn schema_children(schema: &Value) -> impl Iterator<Item = &Value> {
    Draft::Draft7
        .subresources_of(schema)
        .chain(Draft::Draft202012.subresources_of(schema))
        .unique_by(|child| *child as *const Value)
}

/// The absolute URL a `$ref` denotes, resolved against the base in force where
/// it is written.
///
/// This is RFC 3986 reference resolution, which `Url::join` implements: a
/// fragment-only reference names this document, a relative path names a
/// sibling, an absolute URL names itself. A join fails only for a base that
/// cannot be one, which no scheme reaching `fetch_external` produces.
fn reference_url(base: &Url, reference: &str) -> Option<Url> {
    base.join(reference).ok()
}

/// The keywords beside a `$ref` that describe the instance, or `None` when the
/// object carries only the reference and its own identity.
///
/// `$id`, `$anchor` and `$schema` identify the carrier rather than describe an
/// instance, and carrying one into the target would re-base the target's own
/// references onto the carrier's document. `definitions` and `$defs` hold what
/// the reference points *at*, so an object that carries only those — the root
/// shape `pydantic` and `schemars` emit — stays on the fast path.
fn sibling_overlay(schema: &Value) -> Option<Value> {
    const NOT_A_SIBLING: [&str; 7] = [
        "$ref",
        "$id",
        "$anchor",
        "$schema",
        "$comment",
        "definitions",
        "$defs",
    ];

    let map = schema.as_object()?;

    let overlay: serde_json::Map<_, _> = map
        .iter()
        .filter(|(k, _)| !NOT_A_SIBLING.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    (!overlay.is_empty()).then(|| Value::Object(overlay))
}

/// Overlay annotations and recursively combine schema maps. Required keys form a union.
fn merged_over(base: &Value, overlay: &Value) -> Value {
    let (Some(base_map), Some(overlay_map)) = (base.as_object(), overlay.as_object()) else {
        return overlay.clone();
    };

    let mut merged = base_map.clone();

    for (key, value) in overlay_map {
        let merged_value = match (merged.get(key), key.as_str()) {
            (Some(Value::Array(existing)), "required") => {
                let mut union = existing.clone();
                for item in value.as_array().into_iter().flatten() {
                    if !union.contains(item) {
                        union.push(item.clone());
                    }
                }
                Value::Array(union)
            }
            (Some(existing), key) if key != "const" && key != "default" => {
                merged_over(existing, value)
            }
            _ => value.clone(),
        };

        merged.insert(key.clone(), merged_value);
    }

    Value::Object(merged)
}

fn merged_all_of(base: &Value, overlay: &Value) -> Value {
    let mut merged = merged_over(base, overlay);
    if let (Some(left), Some(right)) = (base["enum"].as_array(), overlay["enum"].as_array()) {
        merged["enum"] = Value::Array(
            left.iter()
                .filter(|value| right.contains(value))
                .cloned()
                .collect(),
        );
    }
    for keyword in ["properties", "patternProperties", "$defs", "definitions"] {
        if let (Some(left), Some(right)) = (base[keyword].as_object(), overlay[keyword].as_object())
        {
            for (key, value) in right {
                if let Some(existing) = left.get(key) {
                    merged[keyword][key] = merged_all_of(existing, value);
                }
            }
        }
    }
    merged
}

/// A copy of `schema` whose every `$ref` string is absolute against `base`.
///
/// A subschema evaluated outside its document keeps its `#/...` pointers and
/// loses the document they name, so every reference fails to resolve. The walk
/// covers the subschema's own JSON and follows nothing, so it terminates
/// however cyclic the schema it came from is. A nested `$id` re-bases the
/// references beneath it, as it does in traversal. A reference that cannot be
/// joined is left as written.
fn absolute_refs(schema: &Value, base: &Url) -> Value {
    match schema {
        Value::Object(map) => {
            let absolute_id = schema["$id"].as_str().and_then(|id| base.join(id).ok());
            let base = rebase(base, schema).unwrap_or_else(|| base.clone());

            Value::Object(
                map.iter()
                    .map(|(key, value)| {
                        let value = match (key.as_str(), value.as_str()) {
                            ("$ref" | "$dynamicRef" | "$recursiveRef", Some(reference)) => {
                                reference_url(&base, reference)
                                    .map_or_else(|| value.clone(), |u| Value::String(u.into()))
                            }
                            ("$id", Some(id)) if id.starts_with('#') => value.clone(),
                            ("$id", Some(_)) => absolute_id
                                .as_ref()
                                .map_or_else(|| value.clone(), |id| Value::String(id.to_string())),
                            ("enum" | "const" | "default" | "examples", _) => value.clone(),
                            _ => absolute_refs(value, &base),
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(|i| absolute_refs(i, base)).collect()),
        other => other.clone(),
    }
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

/// Classifies the draft declared by a schema resource.
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

struct CacheSchemaRetriever<E: Environment> {
    cache: Cache<E>,
    pending: Arc<Mutex<Vec<Url>>>,
    documents: Arc<Mutex<HashMap<Url, Arc<Value>>>>,
}

impl<E: Environment> Retrieve for CacheSchemaRetriever<E> {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let url = Url::parse(uri.as_str())?;
        if let Some(schema) = self.documents.lock().get(&url) {
            return Ok(absolute_refs(schema, &url));
        }
        if let Some(schema) = self.cache.get_schema(&url) {
            self.documents.lock().insert(url.clone(), schema.clone());
            return Ok(absolute_refs(&schema, &url));
        }
        if let Some((base, schema)) = self.cache.resource_at(&url) {
            return Ok(absolute_refs(&schema, &base));
        }
        let mut pending = self.pending.lock();
        if !pending.contains(&url) {
            pending.push(url);
        }
        Err(WouldBlockError.into())
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

        match error.kind() {
            ValidationErrorKind::AdditionalProperties { unexpected } => {
                keys = keys.extend(unexpected.iter().map(Key::from).map(KeyOrIndex::Key));
            }
            _ => {}
        }

        'outer: for path in error.instance_path() {
            match path {
                jsonschema::paths::LocationSegment::Property(p) => match node {
                    dom::Node::Table(t) => {
                        let entries = t.entries().read();
                        for (k, entry) in entries.iter() {
                            if k.value() == p.as_ref() {
                                keys = keys.join(k.clone());
                                node = entry.clone();
                                continue 'outer;
                            }
                        }
                        return Err(anyhow!("invalid key"));
                    }
                    _ => return Err(anyhow!("invalid key")),
                },
                jsonschema::paths::LocationSegment::Index(idx) => {
                    node = node.try_get(idx).map_err(|_| anyhow!("invalid index"))?;
                    keys = keys.join(idx);
                }
            }
        }

        Ok(Self { keys, node, error })
    }

    #[must_use]
    pub fn text_ranges(&self) -> Box<dyn Iterator<Item = TextRange> + '_> {
        match self.error.kind() {
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
