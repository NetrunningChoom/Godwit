// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors

fn main() -> Result<(), slint_build::CompileError> {
    // Material is the design language of the official Stoat client.
    let config = slint_build::CompilerConfiguration::new().with_style("material-dark".into());
    slint_build::compile_with_config("ui/app.slint", config)
}
