# Third-party licenses: syntax highlighting

All MIT. The parsers and the highlight/injection queries come from these crates; the queries are the ones shipped with
each grammar. A few are combined or trimmed in `src/lib.rs`: TypeScript and SCSS extend the JavaScript and CSS queries,
C++ extends C, and two patterns are removed from the Markdown block query.

| Component | Crate / source | Version | License file |
|---|---|---|---|
| tree-sitter, tree-sitter-highlight, tree-sitter-language | tree-sitter/tree-sitter | 0.27.1 | `tree-sitter.txt` |
| Rust | tree-sitter-rust | 0.24.2 | `tree-sitter-rust.txt` |
| PHP (+ `injections-text.scm`, inlined) | tree-sitter-php | 0.25.1 | `tree-sitter-php.txt` |
| TypeScript / TSX | tree-sitter-typescript | 0.23.2 | `tree-sitter-typescript.txt` |
| JavaScript / JSX | tree-sitter-javascript | 0.25.0 | `tree-sitter-javascript.txt` |
| Python | tree-sitter-python | 0.25.0 | `tree-sitter-python.txt` |
| JSON | tree-sitter-json | 0.24.8 | `tree-sitter-json.txt` |
| TOML | tree-sitter-toml-ng | 0.7.0 | `tree-sitter-toml-ng.txt` |
| YAML | tree-sitter-yaml | 0.7.2 | `tree-sitter-yaml.txt` |
| CSS | tree-sitter-css | 0.25.0 | `tree-sitter-css.txt` |
| SCSS | tree-sitter-scss | 1.0.0 | `tree-sitter-scss.txt` |
| HTML | tree-sitter-html | 0.23.2 | `tree-sitter-html.txt` |
| Markdown (block + inline) | tree-sitter-md | 0.5.4 | `tree-sitter-md.txt` |
| Bash | tree-sitter-bash | 0.25.1 | `tree-sitter-bash.txt` |
| SQL | tree-sitter-sequel (derekstride/tree-sitter-sql) | 0.3.11 | `tree-sitter-sequel.txt` |
| Dockerfile | tree-sitter-containerfile | 0.9.2 | `tree-sitter-containerfile.txt` |
| C++ (also `.mq4`/`.mq5`/`.mqh`) | tree-sitter-cpp | 0.23.4 | `tree-sitter-cpp.txt` |
| C (highlight query only, for C++) | tree-sitter-c | 0.24.2 | `tree-sitter-c.txt` |
| Twig (+ `injections.scm`, inlined) | gbprod/tree-sitter-twig (git, rev 2208d2a) | 1.1.0 | `tree-sitter-twig.txt` |
