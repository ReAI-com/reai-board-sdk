# Releasing

Release the public SDK only from a clean, reviewed `main` branch.

1. Run `./scripts/check-release-readiness.sh` and the full CI command set.
2. Run `cargo package` and test downstream consumers against the generated
   `.crate` archive.
3. Push the immutable version tag (for example, `v0.3.0`) before publishing.
   README images intentionally use that tag, so the tag must exist first.
4. Run `cargo publish --dry-run`, then `cargo publish` only after explicit
   release approval.
5. Create the matching GitHub Release and verify the crates.io and docs.rs
   pages before closing the tracking issue.

If a tag was pushed but publishing failed, only delete and recreate the tag
when no release or external reference uses it. Otherwise, fix forward with the
next patch version.
