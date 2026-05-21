use casper_types::{
    BlockHash, Digest, EraId, Gas, PublicKey, Timestamp, Transfer, contract_messages::Messages,
    execution::Effects,
};
use schemars::JsonSchema;

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(deny_unknown_fields, rename = "MinimalBlockInfo")]
/// Minimal info about a `Block` needed to satisfy the node status request.
pub(crate) struct MinimalBlockInfoSchema {
    hash: BlockHash,
    timestamp: Timestamp,
    era_id: EraId,
    height: u64,
    state_root_hash: Digest,
    creator: PublicKey,
}

#[allow(dead_code)]
#[derive(JsonSchema)]
#[schemars(rename = "SpeculativeExecutionResult")]
pub(crate) struct SpeculativeExecutionResultSchema {
    /// Block hash against which the execution was performed.
    block_hash: BlockHash,
    /// List of transfers that happened during execution.
    transfers: Vec<Transfer>,
    /// Gas limit.
    limit: Gas,
    /// Gas consumed.
    consumed: Gas,
    /// Execution effects.
    effects: Effects,
    /// Messages emitted during execution.
    messages: Messages,
    /// Did the wasm execute successfully?
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use casper_binary_port::SpeculativeExecutionResult;
    use casper_types::{
        BlockHash, Digest, Gas, Transfer, contract_messages::Messages, execution::Effects,
    };
    use schemars::schema_for;
    use serde_json::json;

    use crate::rpcs::types::SpeculativeExecutionResultSchema;

    #[test]
    pub fn speculative_execution_result_should_validate_against_schema() {
        let ser = SpeculativeExecutionResult::new(
            BlockHash::new(Digest::from([0; Digest::LENGTH])),
            vec![Transfer::example().clone()],
            Gas::zero(),
            Gas::zero(),
            Effects::new(),
            Messages::new(),
            None,
        );
        let schema_struct = schema_for!(SpeculativeExecutionResultSchema);

        let schema = json!(schema_struct);
        let instance = serde_json::to_value(&ser).expect("should json-serialize result");

        let validator = jsonschema::validator_for(&schema).expect("schema should compile");
        let errors = validator
            .iter_errors(&instance)
            .map(|error| format!("{} at {}", error, error.instance_path))
            .collect::<Vec<_>>();

        assert!(
            errors.is_empty(),
            "instance failed schema validation:\n{}\ninstance:\n{}",
            errors.join("\n"),
            serde_json::to_string_pretty(&instance).expect("instance should pretty-print")
        );
    }
}
