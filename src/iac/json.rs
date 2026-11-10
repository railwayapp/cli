use serde_json::{Map, Value};

pub fn clone_value(value: &Value) -> Value {
    value.clone()
}

pub fn as_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

pub fn as_object_mut(value: &mut Value) -> Option<&mut Map<String, Value>> {
    value.as_object_mut()
}

pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.get(name)
}

pub fn field_str<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name).and_then(Value::as_str)
}

pub fn stable_stringify(value: &Value) -> String {
    serde_json::to_string(&sort_for_json(value)).unwrap_or_else(|_| "null".to_string())
}

pub fn sort_for_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_for_json).collect()),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().cloned().collect();
            keys.sort();
            let mut out = Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    out.insert(key, sort_for_json(child));
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

pub fn prune_empty(value: Value) -> Value {
    prune_empty_at(value, &[])
}

fn prune_empty_at(value: Value, path: &[&str]) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|child| prune_empty_at(child, path))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                let mut next_path = path.to_vec();
                next_path.push(key.as_str());
                let child = prune_empty_at(child, &next_path);
                if child.is_null() {
                    continue;
                }
                if let Value::Object(ref inner) = child {
                    let keep_empty = matches!(
                        path.last().copied(),
                        Some("customDomains") | Some("serviceDomains") | Some("tcpProxies")
                    );
                    if inner.is_empty() && !keep_empty {
                        continue;
                    }
                }
                out.insert(key, child);
            }
            Value::Object(out)
        }
        other => other,
    }
}

pub fn merge_objects(base: Value, extra: Value) -> Value {
    match (base, extra) {
        (Value::Object(mut left), Value::Object(right)) => {
            for (key, value) in right {
                left.insert(key, value);
            }
            Value::Object(left)
        }
        (_, extra) => extra,
    }
}
