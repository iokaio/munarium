// SPDX-License-Identifier: Apache-2.0
use munarium_client::{
    server_api::{ApiRequest, ServerApiClient},
    MunariumClientOptions, Result,
};
use serde_json::{json, Value};

async fn scenario(api: ServerApiClient) -> Result<()> {
    let version: Value = api.version_info(ApiRequest::default()).await?.json()?;
    assert_eq!(version["version"], "1.2.0");
    let shape = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: api-rust-docs, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n";
    api.apply_shape(ApiRequest {
        body: shape.as_bytes().to_vec(),
        content_type: "text/yaml".into(),
        ..Default::default()
    })
    .await?;
    let name = format!(
        "api-rust-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let collection: Value = api
        .create_collection(ApiRequest::json(
            &json!({"name":name,"shape_ref":"api-rust-docs@1","access_level":0,"compartments":[]}),
        )?)
        .await?
        .json()?;
    let id = collection["id"].as_str().unwrap();
    let first: Value = api
        .get_collection_vocabulary(ApiRequest::default().with_path("id", id))
        .await?
        .json()?;
    let input = ApiRequest::json(&json!({"revision":first["revision"],"enabled":true,"auto_generate":false,"sampling":null,"groups":[["purchase order","procurement request"]]}))?.with_path("id", id);
    let saved: Value = api
        .replace_collection_vocabulary(input.clone())
        .await?
        .json()?;
    assert!(saved["sampling"].is_null());
    assert!(matches!(
        api.replace_collection_vocabulary(input).await,
        Err(munarium_client::MunariumError::InvalidInput { .. })
    ));
    let disabled: Value = api
        .update_collection_vocabulary(
            ApiRequest::json(&json!({"revision":saved["revision"],"enabled":false}))?
                .with_path("id", id),
        )
        .await?
        .json()?;
    assert_eq!(disabled["enabled"], false);
    let revision: Value = api
        .get_vocabulary_revision(ApiRequest::default().with_path("id", id))
        .await?
        .json()?;
    assert_eq!(revision["revision"], disabled["revision"]);
    assert!(matches!(
        api.get_source(ApiRequest::default().with_path("source_id", "src-absent"))
            .await,
        Err(munarium_client::MunariumError::NotFound { .. })
    ));
    Ok(())
}

pub async fn run(endpoint: &str, token: &str, grpc: bool) -> usize {
    let options = MunariumClientOptions::new(endpoint)
        .token(token)
        .uid("api-rust-conformance");
    let client = if grpc {
        ServerApiClient::grpc(options).await
    } else {
        ServerApiClient::rest(options)
    };
    let result = match client {
        Ok(client) => scenario(client).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => {
            println!("PASS complete API vocabulary and source references (grpc={grpc})");
            0
        }
        Err(error) => {
            eprintln!("FAIL complete API: {error}");
            1
        }
    }
}
