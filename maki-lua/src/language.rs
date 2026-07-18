use arborium::tree_sitter::Language as TsLanguage;

const NAME_ALIASES: &[(&str, &str)] = &[("c_sharp", "c-sharp")];

fn canonical(name: &str) -> &str {
    for &(alias, canonical) in NAME_ALIASES {
        if name == alias {
            return canonical;
        }
    }
    name
}

pub(crate) fn from_name(name: &str) -> Option<TsLanguage> {
    arborium::get_language(canonical(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_c_sharp_via_underscore_alias() {
        assert!(from_name("c_sharp").is_some());
    }

    #[test]
    fn resolves_c_sharp_via_hyphen_canonical() {
        assert!(from_name("c-sharp").is_some());
    }

    #[test]
    fn resolves_all_previously_supported_names() {
        for name in [
            "rust",
            "python",
            "typescript",
            "tsx",
            "javascript",
            "gleam",
            "go",
            "html",
            "java",
            "c",
            "cpp",
            "c_sharp",
            "ruby",
            "php",
            "swift",
            "kotlin",
            "scala",
            "bash",
            "lua",
            "elixir",
            "markdown",
            "starlark",
            "zig",
            "nix",
            "dart",
        ] {
            assert!(from_name(name).is_some(), "language not found: {name}");
        }
    }

    #[test]
    fn resolves_newly_supported_names() {
        for name in [
            "haskell",
            "ocaml",
            "julia",
            "r",
            "perl",
            "clojure",
            "commonlisp",
            "fsharp",
            "erlang",
            "elm",
            "groovy",
            "sql",
            "json",
            "yaml",
            "toml",
            "dockerfile",
            "css",
            "scss",
            "vim",
            "uiua",
            "proto",
            "thrift",
        ] {
            assert!(from_name(name).is_some(), "language not found: {name}");
        }
    }

    #[test]
    fn returns_none_for_unknown_name() {
        assert!(from_name("definitely-not-a-language").is_none());
    }
}
