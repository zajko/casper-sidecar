use std::fmt::{self, Display, Formatter};

use serde_json::{Map, Value};

use crate::error::{Error, ReservedErrorCode};

/// The "params" field of a JSON-RPC request.
///
/// As per [the JSON-RPC specification](https://www.jsonrpc.org/specification#parameter_structures),
/// if present these must be a JSON Array or Object.
///
/// `Params` is effectively a restricted [`serde_json::Value`], and can be converted to a `Value`
/// using `Value::from()` if required.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Params {
    /// Represents a JSON Array.
    Array(Vec<Value>),
    /// Represents a JSON Object.
    Object(Map<String, Value>),
}

impl Params {
    #[allow(clippy::result_large_err)]
    pub(super) fn try_from(params: Value) -> Result<Self, Error> {
        let err_invalid_request = |additional_info: &str| {
            Err(Error::new(
                ReservedErrorCode::InvalidRequest,
                additional_info,
            ))
        };

        match params {
            Value::Null => {
                err_invalid_request("If present, 'params' must be an Array or Object, but was null")
            }
            Value::Bool(false) => err_invalid_request(
                "If present, 'params' must be an Array or Object, but was 'false'",
            ),
            Value::Bool(true) => err_invalid_request(
                "If present, 'params' must be an Array or Object, but was 'true'",
            ),
            Value::Number(_) => err_invalid_request(
                "If present, 'params' must be an Array or Object, but was a Number",
            ),
            Value::String(_) => err_invalid_request(
                "If present, 'params' must be an Array or Object, but was a String",
            ),
            Value::Array(array) => Ok(Params::Array(array)),
            Value::Object(map) => Ok(Params::Object(map)),
        }
    }

    /// Returns `true` if `self` is an Array, otherwise returns `false`.
    #[must_use]
    pub fn is_array(&self) -> bool {
        self.as_array().is_some()
    }

    /// Returns a reference to the inner `Vec` if `self` is an Array, otherwise returns `None`.
    #[must_use]
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Params::Array(array) => Some(array),
            Params::Object(_) => None,
        }
    }

    /// Returns a mutable reference to the inner `Vec` if `self` is an Array, otherwise returns
    /// `None`.
    #[must_use]
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Params::Array(array) => Some(array),
            Params::Object(_) => None,
        }
    }

    /// Returns `true` if `self` is an Object, otherwise returns `false`.
    #[must_use]
    pub fn is_object(&self) -> bool {
        self.as_object().is_some()
    }

    /// Returns a reference to the inner `Map` if `self` is an Object, otherwise returns `None`.
    #[must_use]
    pub fn as_object(&self) -> Option<&Map<String, Value>> {
        match self {
            Params::Object(map) => Some(map),
            Params::Array(_) => None,
        }
    }

    /// Returns a mutable reference to the inner `Map` if `self` is an Object, otherwise returns
    /// `None`.
    #[must_use]
    pub fn as_object_mut(&mut self) -> Option<&mut Map<String, Value>> {
        match self {
            Params::Object(map) => Some(map),
            Params::Array(_) => None,
        }
    }

    /// Returns `true` if `self` is an empty Array or an empty Object, otherwise returns `false`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Params::Array(array) => array.is_empty(),
            Params::Object(map) => map.is_empty(),
        }
    }
}

impl Display for Params {
    fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
        Display::fmt(&Value::from(self.clone()), formatter)
    }
}

/// The default value for `Params` is an empty Array.
impl Default for Params {
    fn default() -> Self {
        Params::Array(Vec::new())
    }
}

impl From<Params> for Value {
    fn from(params: Params) -> Self {
        match params {
            Params::Array(array) => Value::Array(array),
            Params::Object(map) => Value::Object(map),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn should_fail_to_convert_invalid_params(bad_params: Value, expected_data: &str) {
        let error = Params::try_from(bad_params).unwrap_err();
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": -32600,
                "message": "Invalid Request",
                "data": expected_data,
            })
        );
    }

    #[test]
    fn should_fail_to_convert_params_from_null() {
        should_fail_to_convert_invalid_params(
            json!(null),
            "If present, 'params' must be an Array or Object, but was null",
        );
    }

    #[test]
    fn should_fail_to_convert_params_from_false() {
        should_fail_to_convert_invalid_params(
            json!(false),
            "If present, 'params' must be an Array or Object, but was 'false'",
        );
    }

    #[test]
    fn should_fail_to_convert_params_from_true() {
        should_fail_to_convert_invalid_params(
            json!(true),
            "If present, 'params' must be an Array or Object, but was 'true'",
        );
    }

    #[test]
    fn should_fail_to_convert_params_from_a_number() {
        should_fail_to_convert_invalid_params(
            json!(9),
            "If present, 'params' must be an Array or Object, but was a Number",
        );
    }

    #[test]
    fn should_fail_to_convert_params_from_a_string() {
        should_fail_to_convert_invalid_params(
            json!("s"),
            "If present, 'params' must be an Array or Object, but was a String",
        );
    }

    #[test]
    fn should_convert_params_from_an_array() {
        let params = Params::try_from(json!([])).unwrap();
        assert!(matches!(params, Params::Array(v) if v.is_empty()));

        let array = json!([9, false]).as_array().unwrap().clone();
        let params = Params::try_from(json!(array.clone())).unwrap();
        assert!(matches!(params, Params::Array(v) if v == array));
    }

    #[test]
    fn should_convert_params_from_an_object() {
        let params = Params::try_from(json!({})).unwrap();
        assert!(matches!(params, Params::Object(v) if v.is_empty()));

        let map = json!({ "a": 9, "b": false }).as_object().unwrap().clone();
        let params = Params::try_from(json!(map.clone())).unwrap();
        assert!(matches!(params, Params::Object(v) if v == map));
    }
}
