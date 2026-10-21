<p align="center">
  <a href="https://mru.space">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://mru.space/assets/readme/mru-github-dark.gif">
      <img src="https://mru.space/assets/readme/mru-github-light.gif" alt="Mru" width="120" height="120">
    </picture>
  </a>
</p>

# Mru Field

Software for hard-to-reach systems on Earth: ocean observatories, buoys and moorings, offshore and remote sites, polar and seismic stations, anything that runs for months between visits. It runs an operator's task as replicas on the system's own processor, delivers only outputs it has checked, retires replicas that fail, keeps going on the ones left, and resumes after a reset or a power cut. Part of [Mru](https://mru.space).

**Status (October 2026): planned, not built.** This repository holds the [specification](docs/spec.md) of the Mru Field supervisor. The prototype it grows from is the fault-injection demo in [mruspace/flight](https://github.com/mruspace/flight), which ran on ESA's OPS-SAT PRETTY satellite on 18 October 2026 with a fixed task, and which uses the same proved decision rule. Code lands here after the OPS-SAT run 2 package and before the first deployment, an EMSO ERIC ocean test planned for 2027.

## What it does that a service manager does not

systemd restarts a process that dies. The supervisor catches a process that is alive and wrong, alive and hung, or restarted from zero after a reset: it runs the task three times on the same input, compares the outputs on every cycle, and steps down from vote to compare to self-check as replicas fail. It runs under systemd; it does not replace it.

## Layout

| Path | What |
|---|---|
| [docs/spec.md](docs/spec.md) | The specification: the task interface, modes, health, limits, checkpoint, outputs, configuration, the first deployment, acceptance |

## Related

- [mruspace/flight](https://github.com/mruspace/flight): Mru Flight, the same core for spacecraft, with the demo and its results in orbit
- [docs.mru.space](https://docs.mru.space): the shrinking quorum, what is proved and what is tested
- [mruspace/dusk](https://github.com/mruspace/dusk): the reliability simulator

## Questions or contributions

Comments on the specification are welcome as pull requests; see
[CONTRIBUTING.md](CONTRIBUTING.md). Email [contact@mru.space](mailto:contact@mru.space).

## License

Code under [Apache License 2.0](./LICENSE). The **Mru** name and mark are
trademarks; see [TRADEMARK.md](./TRADEMARK.md).
