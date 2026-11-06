// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use protobuf::Enum;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/googlechat.proto");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    // Compact wire metadata for pblite: avoid enabling the full reflection
    // runtime on the large Chat schema just to decode stream events.
    let parsed = protobuf_parse::Parser::new()
        .pure()
        .include("proto")
        .input("proto/googlechat.proto")
        .parse_and_typecheck()?;
    let mut schema =
        String::from("fn fields(name: &str) -> &'static [(u32, i32, &'static str, bool)] { match name {\n");
    fn emit(schema: &mut String, prefix: &str, messages: &[protobuf::descriptor::DescriptorProto]) {
        for message in messages {
            let name = format!("{prefix}.{}", message.name());
            schema.push_str(&format!("{name:?} => &["));
            for field in &message.field {
                schema.push_str(&format!(
                    "({}, {}, {:?}, {}),",
                    field.number(),
                    field.type_().value(),
                    field.type_name(),
                    field.label().value() == 3
                ));
            }
            schema.push_str("],\n");
            emit(schema, &name, &message.nested_type);
        }
    }
    for file in &parsed.file_descriptors {
        emit(&mut schema, "", &file.message_type);
    }
    schema.push_str("_ => &[], } }\n");
    fs::write(out.join("pblite_fields.rs"), schema)?;
    protobuf_codegen::Codegen::new()
        .pure()
        .include("proto")
        .input("proto/googlechat.proto")
        .out_dir(&out)
        .customize(
            protobuf_codegen::Customize::default()
                .lite_runtime(true)
                .gen_mod_rs(false),
        )
        .run()?;
    // include! takes no inner attributes or inner doc comments; the lint
    // allowances live on src/proto.rs instead.
    let path = out.join("googlechat.rs");
    let generated = fs::read_to_string(&path)?;
    let included: Vec<&str> = generated
        .lines()
        .filter(|line| !line.starts_with("#!") && !line.starts_with("//!"))
        .collect();
    fs::write(path, included.join("\n"))?;
    Ok(())
}
