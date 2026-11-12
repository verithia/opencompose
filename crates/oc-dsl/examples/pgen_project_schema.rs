//! JSON types only; semantic validation and rendering remain separate gates.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(oc_dsl::ast::Project);
    println!("{}", serde_json::to_string(&schema)?);
    Ok(())
}
