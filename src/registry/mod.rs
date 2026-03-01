#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct NamespacedKey {
    pub namespace: String,
    pub name: String,
}

impl NamespacedKey {
    pub fn parse(input: &str) -> Option<Self> {
        let raw = input.trim();
        if raw.is_empty() {
            return None;
        }

        if let Some((namespace, name)) = raw.split_once(':') {
            let namespace = namespace.trim();
            let name = name.trim();
            if namespace.is_empty() || name.is_empty() {
                return None;
            }
            return Some(Self {
                namespace: namespace.to_string(),
                name: name.to_string(),
            });
        }

        Some(Self {
            namespace: "core".to_string(),
            name: raw.to_string(),
        })
    }

    pub fn to_string(&self) -> String {
        format!("{}:{}", self.namespace, self.name)
    }
}
