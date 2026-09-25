// SPDX-License-Identifier: Apache-2.0
//! Invoked only by tools/test-datastore-permissions.ps1 in disposable containers.
use super::*;
use std::{fs, path::Path};

#[test]
#[ignore = "container image preparation only"]
fn prepare() {
    let root = Path::new("/qualification");
    fs::create_dir(root).unwrap();
    let (_, id) = build_into(&root.join("artifact"));
    fs::write(root.join("artifact-id"), id).unwrap();
    let (corrupt, _) = build_into(&root.join("corrupt"));
    corrupt
        .put_component(MANIFEST, b"corrupt manifest")
        .unwrap();
    build_into(&root.join("denied/artifact"));
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root.join("denied"), fs::Permissions::from_mode(0o600)).unwrap();
}

fn scratch_entries() -> Vec<String> {
    let mut entries = fs::read_dir("/scratch")
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

#[test]
#[ignore = "requires the restricted Docker serving environment"]
fn serve() {
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let uid = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .unwrap();
    assert!(uid.split_whitespace().skip(1).all(|value| value == "65532"));
    assert!(status
        .lines()
        .any(|line| line == "CapEff:\t0000000000000000"));
    // Probe the actual mounts; an ordinary writable container cannot pass.
    assert_eq!(
        fs::write("/root-write-probe", b"x")
            .unwrap_err()
            .raw_os_error(),
        Some(30)
    );
    assert_eq!(
        fs::write("/qualification/artifact/write-probe", b"x")
            .unwrap_err()
            .raw_os_error(),
        Some(30)
    );
    fs::write("/scratch/sentinel", b"not owned by the shard").unwrap();
    let before = scratch_entries();
    let id = fs::read_to_string("/qualification/artifact-id").unwrap();
    let case = std::env::var("PERMISSIONS_CASE").unwrap();
    if case == "denied-traversal" {
        let err = fs::read("/qualification/denied/artifact/manifest.json").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(LocalFileStore::new("/qualification/denied/artifact").is_err());
    } else {
        let root = if case == "corrupt" {
            "/qualification/corrupt"
        } else {
            "/qualification/artifact"
        };
        let store = LocalFileStore::new(root).unwrap();
        let open = || OpenShard::open(&store, &id, &ReaderCapabilities::v1(), &Limits::default());
        match case.as_str() {
            "serve" => {
                assert_eq!(std::env::temp_dir(), Path::new("/scratch"));
                for _ in 0..2 {
                    let shard = open().unwrap();
                    assert!(
                        scratch_entries().len() > before.len(),
                        "scratch must survive while mmaps are live"
                    );
                    assert_eq!(shard.records().len(), 3);
                    let query = munarium_datastore::lexical::LexicalPlan {
                        terms: shard
                            .analyze("congress")
                            .unwrap()
                            .into_iter()
                            .map(munarium_datastore::lexical::PlanTerm::user)
                            .collect(),
                        minimum_should_match: 1,
                        ..Default::default()
                    };
                    let mut ids = shard
                        .lexical_candidates(&query, 3)
                        .unwrap()
                        .into_iter()
                        .map(|c| c.chunk_id)
                        .collect::<Vec<_>>();
                    ids.sort();
                    assert_eq!(ids, ["s1#0", "s1#1"]);
                    assert_eq!(
                        shard.vector_candidates(&[1.0, 0.0, 0.0], 1).unwrap()[0].chunk_id,
                        "s1#0"
                    );
                    drop(shard);
                    assert_eq!(
                        scratch_entries(),
                        before,
                        "drop must remove only shard-owned scratch"
                    );
                }
            }
            "missing-scratch" | "readonly-scratch" => {
                let err = open().unwrap_err();
                match err {
                    munarium_datastore::Error::Io(err) => {
                        if case == "missing-scratch" {
                            assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
                        } else {
                            assert_eq!(err.kind(), std::io::ErrorKind::ReadOnlyFilesystem);
                        }
                    }
                    other => panic!("expected scratch IO refusal, got {other:?}"),
                }
            }
            "corrupt" => assert!(matches!(
                open(),
                Err(munarium_datastore::Error::Integrity(_))
            )),
            other => panic!("unknown case {other}"),
        }
    }
    assert_eq!(scratch_entries(), before);
    assert_eq!(
        fs::read("/scratch/sentinel").unwrap(),
        b"not owned by the shard"
    );
}
