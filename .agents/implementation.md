# Repository Guidance

Read `readme.md` for the current status. Before planning or changing behavior, discover and read applicable `design/*.md`. Use `base-design-review` to assess plans and changes, and `base-design-author` when user requirements are introduced, changed, or removed. Review design compliance before opening or updating a pull request.

Install shared guidance with `cargo bake agent:context:install`; discover it with `cargo bake agent:context:list` and `cargo bake agent:context:show`. Keep private task tooling out of the library's dependency graph.

Use `base-security-audit` for security reviews and record them in `security/audit-*.md`. Use `base-security-advisory` for vulnerability reports and potentially advisory-worthy findings. Read relevant `// SECURITY:` comments and `design/*.md` before reporting a security finding.
