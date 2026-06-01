mod docgen;
mod helpers;
mod path;

use std::{env, error::Error};

type DynError = Box<dyn Error>;

pub mod tasks {
    use crate::DynError;
    use std::collections::HashSet;

    pub fn docgen() -> Result<(), DynError> {
        use crate::docgen::*;
        write(TYPABLE_COMMANDS_MD_OUTPUT, &typable_commands()?);
        write(STATIC_COMMANDS_MD_OUTPUT, &static_commands()?);
        write(LANG_SUPPORT_MD_OUTPUT, &lang_features()?);
        Ok(())
    }

    pub fn querycheck(languages: impl Iterator<Item = String>) -> Result<(), DynError> {
        use helix_core::syntax::LanguageData;

        let languages_to_check: HashSet<_> = languages.collect();
        let loader = helix_core::config::default_lang_loader();
        for (_language, lang_data) in loader.languages() {
            if !languages_to_check.is_empty()
                && !languages_to_check.contains(&lang_data.config().language_id)
            {
                continue;
            }
            let config = lang_data.config();
            let Some(syntax_config) = LanguageData::compile_syntax_config(config, &loader)? else {
                continue;
            };
            let grammar = syntax_config.grammar;
            LanguageData::compile_indent_query(grammar, config)?;
            LanguageData::compile_textobject_query(grammar, config)?;
            LanguageData::compile_tag_query(grammar, config)?;
            LanguageData::compile_rainbow_query(grammar, config)?;
        }

        println!("Query check succeeded");

        Ok(())
    }

    pub fn themecheck(themes: impl Iterator<Item = String>) -> Result<(), DynError> {
        use helix_view::theme::Loader;

        let themes_to_check: HashSet<_> = themes.collect();

        let theme_names = [
            vec!["default".to_string(), "base16_default".to_string()],
            Loader::read_names(&crate::path::themes()),
        ]
        .concat();
        let loader = Loader::new(&[crate::path::runtime()]);
        let mut errors_present = false;

        for name in theme_names {
            if !themes_to_check.is_empty() && !themes_to_check.contains(&name) {
                continue;
            }

            let (_, warnings) = loader.load_with_warnings(&name).unwrap();

            if !warnings.is_empty() {
                errors_present = true;
                println!("Theme '{name}' loaded with errors:");
                for warning in warnings {
                    println!("\t* {}", warning);
                }
            }
        }

        match errors_present {
            true => Err("Errors found when loading bundled themes".into()),
            false => {
                println!("Theme check successful!");
                Ok(())
            }
        }
    }

    pub fn print_help() {
        println!(
            "
Usage: Run with `cargo xtask <task>`, eg. `cargo xtask docgen`.

    Tasks:
        docgen                     Generate files to be included in the mdbook output.
        query-check [languages]    Check that tree-sitter queries are valid for the given
                                   languages, or all languages if none are specified.
        theme-check [themes]       Check that the theme files in runtime/themes/ are valid for the
                                   given themes, or all themes if none are specified.
        install                    Build a release 'helix' binary + runtime in ../bin/ following
                                   the official packaging guide (sets HELIX_DEFAULT_RUNTIME so
                                   it works like a normal install without needing HELIX_RUNTIME).
"
        );
    }

    pub fn install() -> Result<(), DynError> {
        let bin_dir = std::path::PathBuf::from("../bin");
        let runtime_target = bin_dir.join("runtime");
        let binary_target = bin_dir.join("helix");

        // Compute absolute path for HELIX_DEFAULT_RUNTIME (this gets baked into the binary
        // at compile time via std::option_env!, following the official packaging guide).
        let runtime_abs = std::fs::canonicalize(&runtime_target)
            .unwrap_or_else(|_| {
                // Fallback for older Rust: join with current_dir
                std::env::current_dir()
                    .map(|cwd| cwd.join(&runtime_target))
                    .unwrap_or_else(|_| runtime_target.clone())
            });

        println!("=== treelix custom install (following official packaging guide) ===");
        println!("  Target binary : {}", binary_target.display());
        println!("  Target runtime: {}", runtime_target.display());
        println!("  HELIX_DEFAULT_RUNTIME (baked in): {}", runtime_abs.display());
        println!();

        std::fs::create_dir_all(&bin_dir)?;
        std::fs::create_dir_all(&runtime_target)?;

        // Copy the entire runtime directory (grammars, queries, themes, etc.)
        // This is the key step that normal packages do.
        println!("Copying runtime/ into place...");
        let cp_status = std::process::Command::new("cp")
            .args(["-r", "runtime/.", runtime_target.to_str().unwrap()])
            .status()?;
        if !cp_status.success() {
            return Err("Failed to copy runtime directory".into());
        }

        // Set the variable for this build so it gets compiled into the binary
        // (this is exactly what the packaging guide in book/src/building-from-source.md recommends).
        std::env::set_var("HELIX_DEFAULT_RUNTIME", &runtime_abs);

        println!("Building release binary with HELIX_DEFAULT_RUNTIME baked in...");
        let build_status = std::process::Command::new("cargo")
            .args(["build", "--release", "-p", "helix-term"])
            .env("HELIX_DEFAULT_RUNTIME", &runtime_abs)
            .status()?;
        if !build_status.success() {
            return Err("Release build failed".into());
        }

        // Install the binary as "helix" (so typing `helix` runs your custom build)
        println!("Installing binary as 'helix'...");
        std::fs::copy("target/release/hx", &binary_target)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_target)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_target, perms)?;
        }

        println!();
        println!("✅ Successfully installed custom Helix build:");
        println!("   Binary : {}", binary_target.display());
        println!("   Runtime: {}", runtime_target.display());
        println!();
        println!("Because we followed the packaging guide (set HELIX_DEFAULT_RUNTIME at build time");
        println!("and placed runtime/ next to the binary), this `helix` should work like a normal");
        println!("installed version — no need to set HELIX_RUNTIME manually for colors/LSP/etc.");
        println!();
        println!("Make sure '{}' is early in your PATH if you want `helix` to prefer this build.", bin_dir.display());

        Ok(())
    }
}

fn main() -> Result<(), DynError> {
    let mut args = env::args().skip(1);
    let task = args.next();
    match task {
        None => tasks::print_help(),
        Some(t) => match t.as_str() {
            "docgen" => tasks::docgen()?,
            "query-check" => tasks::querycheck(args)?,
            "theme-check" => tasks::themecheck(args)?,
            "install" => tasks::install()?,
            invalid => return Err(format!("Invalid task name: {}", invalid).into()),
        },
    };
    Ok(())
}
