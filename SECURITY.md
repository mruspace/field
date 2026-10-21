# Security

## Reporting a problem

Email [contact@mru.space](mailto:contact@mru.space) with "security" in the
subject. Please do not open a public issue for a security problem. We reply
by email.

## Scope

Mru Field has no qualification evidence. Version 0.1.0 has run on the bench
only, on general-purpose processors. Do not use it to control anything that
can cause harm.

In scope:

- A way to make the supervisor deliver an output that the replicas did not
  agree on in the current mode (two in vote or compare, two computations in
  self-check).
- A way to make the supervisor hang, stop answering SIGTERM, or write a
  checkpoint it cannot resume from.
- A way for a task process to escape the limits the supervisor sets on it,
  or to outlive the supervisor.
- A resume that repeats an event in the log or forgets a retired replica.

Out of scope: faults inside the operator's task that keep its output
deterministic and wrong on every replica alike. The supervisor compares
replicas; it does not know the right answer beyond the known-answer test.

## Supported versions

Only the latest release gets fixes.
