// SPDX-License-Identifier: Apache-2.0
//! Keep the retention inventory and its negative controls in workspace CI.

use std::path::Path;
use std::process::Command;

#[test]
fn retention_inventory_and_negative_controls() {
    let server = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let candidates = std::env::var("RETENTION_PYTHON")
        .map(|interpreter| vec![interpreter])
        .unwrap_or_else(|_| vec!["python3".into(), "python".into(), "py".into()]);
    let interpreter = candidates
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .args(["-c", "import sys; sys.exit(sys.version_info < (3, 9))"])
                .output()
                .is_ok_and(|result| result.status.success())
        })
        .expect("retention inventory needs Python >= 3.9 (or RETENTION_PYTHON)");
    let output = Command::new(interpreter)
        .current_dir(server)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "tools",
            "-p",
            "test_retention_inventory.py",
        ])
        .output()
        .expect("run retention inventory negative controls");
    assert!(
        output.status.success(),
        "retention inventory failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
