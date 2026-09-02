//! Print the OpenAPI 3.1 spec to stdout as pretty JSON.
//!
//! Used by the frontend to regenerate `lib/api-types.gen.ts` without
//! needing a running server:
//!
//!     cargo run --quiet --bin dump-openapi > /tmp/vela-openapi.json
//!     (cd frontend && npx openapi-typescript /tmp/vela-openapi.json -o lib/api-types.gen.ts)
//!
//! Also useful for API-first CI checks against a schema fixture.

fn main() {
    let spec = api::openapi::openapi_spec();
    match serde_json::to_string_pretty(&spec) {
        Ok(s) => println!("{}", s),
        Err(e) => {
            eprintln!("failed to serialise openapi spec: {e}");
            std::process::exit(1);
        }
    }
}
