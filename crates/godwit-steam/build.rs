// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(env::var("OUT_DIR")?);
    for name in ["voice", "friendslist"] {
        println!("cargo:rerun-if-changed=proto/{name}.proto");
        protobuf_codegen::Codegen::new()
            .pure()
            .include("proto")
            .input(format!("proto/{name}.proto"))
            .out_dir(&out)
            .customize(
                protobuf_codegen::Customize::default()
                    .lite_runtime(true)
                    .gen_mod_rs(false),
            )
            .run()?;

        let path = out.join(format!("{name}.rs"));
        // Match the existing runtime re-export rather than adding a second runtime.
        let generated = fs::read_to_string(&path)?.replace("::protobuf::", "::steam_vent_proto_common::protobuf::");
        // include! cannot accept inner attributes or inner doc comments. Their
        // lint allowances stay on the checked-in wrappers (voice::proto,
        // favourites::proto) instead.
        let included = generated
            .lines()
            .filter(|line| !line.starts_with("#!") && !line.starts_with("//!"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, included)?;
    }
    Ok(())
}
