// SPDX-License-Identifier: Apache-2.0
//! Deterministic gRPC reference generator: decodes the crate's embedded
//! FILE_DESCRIPTOR_SET and emits docs/api/grpc-reference.md. No external
//! protoc-gen-doc plugin — the descriptor set (with source comments) is
//! already compiled into munarium-proto.
//!
//!   cargo run -p munarium-proto --bin gen-grpc-docs -- docs/api/grpc-reference.md
//!
//! Without an argument the markdown goes to stdout. Output is byte-stable for
//! the CI drift check (LF line endings, no BOM).

use prost::Message;
use prost_types::field_descriptor_proto::{Label, Type};
use prost_types::{
    DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, FileDescriptorProto,
    FileDescriptorSet, ServiceDescriptorProto,
};
use std::collections::HashMap;
use std::fmt::Write as _;

fn main() {
    let set = FileDescriptorSet::decode(munarium_proto::FILE_DESCRIPTOR_SET)
        .expect("embedded descriptor set decodes");
    let mut out = String::new();
    out.push_str("# MMP v1 gRPC reference\n\n");
    out.push_str(
        "> Generated from `proto/mmp/v1/` by `cargo run -p munarium-proto --bin gen-grpc-docs -- \
         docs/api/grpc-reference.md`.\n> Do not edit by hand — CI drift-checks this file against \
         the protos.\n\nThe proto files are normative; see [grpc.md](grpc.md) for connection and \
         metadata conventions.\n",
    );

    for file in &set.file {
        let name = file.name();
        if !name.starts_with("mmp/") {
            continue; // google well-known imports
        }
        if name.ends_with("admin.proto") {
            out.push_str(
                "\n## mmp/v1/admin.proto\n\n**Reserved — declared, not served.** No server \
                 routes exist for `AdminService`; it is excluded from client libraries and this \
                 reference until it ships.\n",
            );
            continue;
        }
        render_file(&mut out, file);
    }

    match std::env::args().nth(1) {
        Some(path) => {
            std::fs::write(&path, out.as_bytes()).unwrap_or_else(|e| panic!("write {path}: {e}"))
        }
        None => print!("{out}"),
    }
}

/// Comments by source-info path (e.g. [6, s, 2, m] = service s method m).
/// Leading and trailing (same-line `// ...`) comments both count — field
/// docs in these protos use either style.
fn comment_map(file: &FileDescriptorProto) -> HashMap<Vec<i32>, String> {
    let mut map = HashMap::new();
    if let Some(info) = &file.source_code_info {
        for loc in &info.location {
            let text = [loc.leading_comments(), loc.trailing_comments()]
                .iter()
                .map(|c| clean_comment(c))
                .filter(|c| !c.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !text.trim().is_empty() {
                map.insert(loc.path.clone(), text);
            }
        }
    }
    map
}

fn clean_comment(raw: &str) -> String {
    raw.lines()
        .map(|l| l.strip_prefix(' ').unwrap_or(l).trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// One-line form for table cells.
fn cell_comment(map: &HashMap<Vec<i32>, String>, path: &[i32]) -> String {
    map.get(path)
        .map(|c| c.replace('\n', " ").replace('|', "\\|"))
        .unwrap_or_default()
}

fn render_file(out: &mut String, file: &FileDescriptorProto) {
    let comments = comment_map(file);
    let _ = write!(out, "\n## {}\n", file.name());

    for (si, service) in file.service.iter().enumerate() {
        render_service(out, service, &comments, si as i32);
    }
    for (mi, message) in file.message_type.iter().enumerate() {
        render_message(out, message, &comments, &[4, mi as i32], message.name());
    }
    for (ei, en) in file.enum_type.iter().enumerate() {
        render_enum(out, en, &comments, &[5, ei as i32], en.name());
    }
}

fn render_service(
    out: &mut String,
    service: &ServiceDescriptorProto,
    comments: &HashMap<Vec<i32>, String>,
    index: i32,
) {
    let _ = write!(out, "\n### service {}\n\n", service.name());
    if let Some(c) = comments.get(&vec![6, index]) {
        let _ = write!(out, "{c}\n\n");
    }
    out.push_str("| RPC | Request | Response | Notes |\n|---|---|---|---|\n");
    for (mi, method) in service.method.iter().enumerate() {
        let req = format!(
            "{}{}",
            if method.client_streaming() {
                "stream "
            } else {
                ""
            },
            short(method.input_type())
        );
        let resp = format!(
            "{}{}",
            if method.server_streaming() {
                "stream "
            } else {
                ""
            },
            short(method.output_type())
        );
        let note = cell_comment(comments, &[6, index, 2, mi as i32]);
        let _ = writeln!(out, "| {} | {req} | {resp} | {note} |", method.name());
    }
}

fn render_message(
    out: &mut String,
    message: &DescriptorProto,
    comments: &HashMap<Vec<i32>, String>,
    path: &[i32],
    qualified_name: &str,
) {
    let _ = write!(out, "\n### message {qualified_name}\n\n");
    if let Some(c) = comments.get(path) {
        let _ = write!(out, "{c}\n\n");
    }
    if message.field.is_empty() {
        out.push_str("_(no fields)_\n");
    } else {
        out.push_str("| # | Field | Type | Notes |\n|---|---|---|---|\n");
        for (fi, field) in message.field.iter().enumerate() {
            let mut field_path = path.to_vec();
            field_path.extend([2, fi as i32]);
            let mut note = cell_comment(comments, &field_path);
            if let Some(oneof) = field.oneof_index {
                if !field.proto3_optional() {
                    let oneof_name = message
                        .oneof_decl
                        .get(oneof as usize)
                        .map(|o| o.name())
                        .unwrap_or("oneof");
                    let tag = format!("(oneof `{oneof_name}`) ");
                    note = format!("{tag}{note}");
                }
            }
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                field.number(),
                field.name(),
                field_type(field),
                note
            );
        }
    }
    // nested types (skip synthetic map entries)
    for (ni, nested) in message.nested_type.iter().enumerate() {
        if nested.options.as_ref().is_some_and(|o| o.map_entry()) {
            continue;
        }
        let mut nested_path = path.to_vec();
        nested_path.extend([3, ni as i32]);
        render_message(
            out,
            nested,
            comments,
            &nested_path,
            &format!("{qualified_name}.{}", nested.name()),
        );
    }
    for (ei, en) in message.enum_type.iter().enumerate() {
        let mut enum_path = path.to_vec();
        enum_path.extend([4, ei as i32]);
        render_enum(
            out,
            en,
            comments,
            &enum_path,
            &format!("{qualified_name}.{}", en.name()),
        );
    }
}

fn render_enum(
    out: &mut String,
    en: &EnumDescriptorProto,
    comments: &HashMap<Vec<i32>, String>,
    path: &[i32],
    qualified_name: &str,
) {
    let _ = write!(out, "\n### enum {qualified_name}\n\n");
    if let Some(c) = comments.get(path) {
        let _ = write!(out, "{c}\n\n");
    }
    out.push_str("| Value | # | Notes |\n|---|---|---|\n");
    for (vi, value) in en.value.iter().enumerate() {
        let mut value_path = path.to_vec();
        value_path.extend([2, vi as i32]);
        let note = cell_comment(comments, &value_path);
        let _ = writeln!(out, "| {} | {} | {} |", value.name(), value.number(), note);
    }
}

fn field_type(field: &FieldDescriptorProto) -> String {
    let base = match field.r#type() {
        Type::Message | Type::Enum | Type::Group => short(field.type_name()),
        Type::Double => "double".into(),
        Type::Float => "float".into(),
        Type::Int64 => "int64".into(),
        Type::Uint64 => "uint64".into(),
        Type::Int32 => "int32".into(),
        Type::Fixed64 => "fixed64".into(),
        Type::Fixed32 => "fixed32".into(),
        Type::Bool => "bool".into(),
        Type::String => "string".into(),
        Type::Bytes => "bytes".into(),
        Type::Uint32 => "uint32".into(),
        Type::Sfixed32 => "sfixed32".into(),
        Type::Sfixed64 => "sfixed64".into(),
        Type::Sint32 => "sint32".into(),
        Type::Sint64 => "sint64".into(),
    };
    if field.proto3_optional() {
        format!("optional {base}")
    } else if field.label() == Label::Repeated {
        format!("repeated {base}")
    } else {
        base
    }
}

/// ".mmp.v1.Claim" -> "Claim"; ".google.protobuf.Timestamp" -> "google.protobuf.Timestamp".
fn short(type_name: &str) -> String {
    let t = type_name.trim_start_matches('.');
    match t.strip_prefix("mmp.v1.") {
        Some(rest) => rest.to_string(),
        None => t.to_string(),
    }
}
