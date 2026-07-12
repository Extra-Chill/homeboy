# Check

`homeboy check` provides one entrypoint for Homeboy quality gates. Each gate
uses the same arguments and implementation as its existing command.

```sh
homeboy check audit <component> [audit options]
homeboy check lint <component> [lint options]
homeboy check test <component> [test options]
homeboy check build <component> [build options]
homeboy check review <component> [review options]
```

The existing gate commands remain available. `check` is a dispatching
entrypoint and does not change gate behavior or output.

Related:

- [Review](review.md)
- [Commands index](commands-index.md)
