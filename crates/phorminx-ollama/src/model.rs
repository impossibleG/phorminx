use std::fmt;

use serde::Deserialize;

const MAX_MODEL_NAME_BYTES: usize = 256;

/// A validated Ollama model name, such as `qwen2.5:3b`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModelName(String);

impl ModelName {
    pub fn parse(value: impl Into<String>) -> Result<Self, ModelSelectionError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(ModelSelectionError::InvalidName(
                "model name cannot be empty".to_owned(),
            ));
        }
        if trimmed.len() > MAX_MODEL_NAME_BYTES {
            return Err(ModelSelectionError::InvalidName(format!(
                "model name exceeds {MAX_MODEL_NAME_BYTES} bytes"
            )));
        }
        if trimmed.chars().any(char::is_control) {
            return Err(ModelSelectionError::InvalidName(
                "model name cannot contain control characters".to_owned(),
            ));
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModelName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Optional metadata reported by `/api/tags`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ModelDetails {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub families: Option<Vec<String>>,
    #[serde(default)]
    pub parameter_size: Option<String>,
    #[serde(default)]
    pub quantization_level: Option<String>,
}

/// A locally installed model reported by Ollama.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OllamaModel {
    pub name: ModelName,
    pub modified_at: Option<String>,
    pub size: Option<u64>,
    pub digest: Option<String>,
    pub details: ModelDetails,
}

/// An explicit policy for choosing one model from a discovered catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionPolicy {
    Exact(ModelName),
    /// Try names in caller-defined priority order.
    Preferred(Vec<ModelName>),
    /// Choose the lexicographically first installed model.
    FirstAvailable,
}

impl SelectionPolicy {
    pub fn exact(name: impl Into<String>) -> Result<Self, ModelSelectionError> {
        Ok(Self::Exact(ModelName::parse(name)?))
    }

    pub fn preferred<I, S>(names: I) -> Result<Self, ModelSelectionError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let names = names
            .into_iter()
            .map(|name| ModelName::parse(name.into()))
            .collect::<Result<Vec<_>, _>>()?;
        if names.is_empty() {
            return Err(ModelSelectionError::EmptyPreferenceList);
        }
        Ok(Self::Preferred(names))
    }
}

/// A deterministic snapshot of models installed in Ollama.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelCatalog {
    models: Vec<OllamaModel>,
}

impl ModelCatalog {
    pub(crate) fn from_api(models: Vec<ApiModel>) -> Result<Self, ModelSelectionError> {
        let mut converted = models
            .into_iter()
            .map(OllamaModel::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        converted.sort_by(|left, right| left.name.cmp(&right.name));
        converted.dedup_by(|left, right| left.name == right.name);
        Ok(Self { models: converted })
    }

    pub fn models(&self) -> &[OllamaModel] {
        &self.models
    }

    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    pub fn select(&self, policy: &SelectionPolicy) -> Result<&OllamaModel, ModelSelectionError> {
        match policy {
            SelectionPolicy::Exact(name) => self
                .models
                .iter()
                .find(|model| &model.name == name)
                .ok_or_else(|| ModelSelectionError::NotInstalled(name.clone())),
            SelectionPolicy::Preferred(names) => names
                .iter()
                .find_map(|name| self.models.iter().find(|model| &model.name == name))
                .ok_or(ModelSelectionError::NoPreferredModel),
            SelectionPolicy::FirstAvailable => self
                .models
                .first()
                .ok_or(ModelSelectionError::NoModelsInstalled),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TagsResponse {
    #[serde(default)]
    pub models: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiModel {
    pub name: String,
    #[serde(default)]
    pub modified_at: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub digest: Option<String>,
    #[serde(default)]
    pub details: ModelDetails,
}

impl TryFrom<ApiModel> for OllamaModel {
    type Error = ModelSelectionError;

    fn try_from(model: ApiModel) -> Result<Self, Self::Error> {
        Ok(Self {
            name: ModelName::parse(model.name)?,
            modified_at: model.modified_at,
            size: model.size,
            digest: model.digest,
            details: model.details,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelSelectionError {
    #[error("invalid Ollama model name: {0}")]
    InvalidName(String),
    #[error("the preferred-model list cannot be empty")]
    EmptyPreferenceList,
    #[error("Ollama model `{0}` is not installed")]
    NotInstalled(ModelName),
    #[error("none of the preferred Ollama models are installed")]
    NoPreferredModel,
    #[error("Ollama has no installed models")]
    NoModelsInstalled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_model(name: &str) -> ApiModel {
        ApiModel {
            name: name.to_owned(),
            modified_at: None,
            size: None,
            digest: None,
            details: ModelDetails::default(),
        }
    }

    #[test]
    fn model_names_are_trimmed_and_validated() {
        assert_eq!(
            ModelName::parse(" qwen2.5:3b ").unwrap().as_str(),
            "qwen2.5:3b"
        );
        assert!(ModelName::parse(" ").is_err());
        assert!(ModelName::parse("bad\nname").is_err());
        assert!(ModelName::parse("x".repeat(257)).is_err());
    }

    #[test]
    fn catalogs_are_sorted_deduplicated_and_selected_explicitly() {
        let catalog = ModelCatalog::from_api(vec![
            api_model("zeta:latest"),
            api_model("alpha:1b"),
            api_model("zeta:latest"),
        ])
        .unwrap();
        assert_eq!(catalog.models().len(), 2);
        assert_eq!(
            catalog
                .select(&SelectionPolicy::FirstAvailable)
                .unwrap()
                .name
                .as_str(),
            "alpha:1b"
        );
        let preferred = SelectionPolicy::preferred(["missing:1b", "zeta:latest"]).unwrap();
        assert_eq!(
            catalog.select(&preferred).unwrap().name.as_str(),
            "zeta:latest"
        );
        let exact = SelectionPolicy::exact("alpha:1b").unwrap();
        assert_eq!(catalog.select(&exact).unwrap().name.as_str(), "alpha:1b");
    }

    #[test]
    fn selection_failures_are_typed() {
        let catalog = ModelCatalog::default();
        assert_eq!(
            catalog.select(&SelectionPolicy::FirstAvailable),
            Err(ModelSelectionError::NoModelsInstalled)
        );
        assert!(matches!(
            SelectionPolicy::preferred(Vec::<String>::new()),
            Err(ModelSelectionError::EmptyPreferenceList)
        ));
    }
}
