# Feature development with tasqx

The pattern: one sub-project per feature, tasks ordered by dependencies, context in
annotations. It replaces a board for solo work — the dependency graph does the sprint
grooming for you.

## Set up the feature

A feature is a sub-project under your product, named with a dot:

```console
tasqx init myapp                 # once, for the product
tasqx init myapp.checkout        # the feature
```

Capture the work, then wire the order. A task with an unresolved dependency is
*blocked* and stays out of your working set until its turn comes:

```console
tasqx add "Design the checkout data model" project:myapp.checkout !high est:4h
tasqx add "Implement cart-to-order endpoint" project:myapp.checkout !high est:6h
tasqx add "Payment provider integration" project:myapp.checkout est:1d
tasqx dep 2 1                    # #2 waits for #1
tasqx dep 3 2                    # #3 waits for #2
```

## Put the context on the task

Annotations are stored verbatim — newlines, markdown, code blocks and all. This is
where acceptance criteria, links and implementation notes live:

```console
tasqx annotate 2 '## Acceptance criteria
- [ ] POST /api/orders returns 201 + order id
- [ ] Prices recomputed server-side; client prices are a hint, never truth
- [ ] Idempotency-key header prevents duplicate orders on retry

See ADR-012 for the locking decision.'
```

`tasqx show 2` prints it back, unmangled.

## Work it, one task at a time

```console
tasqx next                       # the single highest-urgency unblocked task
tasqx start 1
tasqx done 1                     # prints: unblocked #2
```

`done` announces what its completion released — you never have to work out what is
next by hand. `tasqx list project:myapp.checkout` shows the whole feature;
`tasqx report project:myapp.checkout` totals the remaining estimates.

## Where this stops

tasqx is single-user and local. There are no assignees, no sprints, no boards. The
moment a second person needs to work the same backlog, you have outgrown this
pattern.
