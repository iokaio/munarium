// SPDX-License-Identifier: Apache-2.0
#[path = "support/authority.rs"]
mod support;
use munarium_store_mem::platform_authority::MemAuthorityStore;

#[tokio::test]
async fn platform_authority_memory_contract() {
    support::contract(&MemAuthorityStore::new("authority-test"), "authority-test").await;
}
