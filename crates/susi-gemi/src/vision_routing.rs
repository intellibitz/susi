//! Route image inputs to vision-capable models.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisionModel {
    pub name: String,
    pub vision: bool,
}

/// Pick the first vision-capable model when the prompt has images.
#[must_use]
pub fn route_vision(has_images: bool, models: &[VisionModel]) -> Option<String> {
    if !has_images {
        return models.first().map(|m| m.name.clone());
    }
    models.iter().find(|m| m.vision).map(|m| m.name.clone())
}

#[cfg(test)]
mod vision_routing_tests {
    use super::*;

    #[test]
    fn vision_routing_requires_vision_model() {
        let models = [
            VisionModel {
                name: "text".into(),
                vision: false,
            },
            VisionModel {
                name: "vl".into(),
                vision: true,
            },
        ];
        assert_eq!(route_vision(true, &models).as_deref(), Some("vl"));
        assert_eq!(route_vision(false, &models).as_deref(), Some("text"));
    }
}
