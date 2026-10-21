# Contributing to Mru Field

Thank you for your interest. Mru Field is version 0.1.0: the supervisor is
built and tested on the bench, on general-purpose processors. It has not run
on a board in a system yet, and it has no qualification evidence. Pull
requests are welcome. For questions or bug reports, email
contact@mru.space.

## Before you open a pull request

Run the same checks as CI:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --release
./scripts/scenarios.sh
./scripts/power-cut.sh
```

The decision rule is the `quorum` crate from
[mruspace/flight](https://github.com/mruspace/flight), pinned by tag in
`Cargo.toml`. Its Kani proofs run in that repository's CI; this repository
adds no decision logic.

## Rules for changes

- `main` is protected. Make every change through a branch and a pull request.
- The supervisor never delivers an output it has not checked. A change to
  the cycle needs a scenario check or an integration test that shows the
  old behavior was wrong.
- Every number in the README comes from a real run. If a change moves a
  number, run the command again and paste what it printed. Do not edit
  numbers by hand.
- State what exists. State plans as "planned". Every number has a source
  and a scope.
- Straight apostrophes, no em dashes, US spelling. Short sentences, limits
  next to claims.
- Keep README, LICENSE and TRADEMARK current.

## Questions

Email [contact@mru.space](mailto:contact@mru.space).

## License

By contributing, you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
