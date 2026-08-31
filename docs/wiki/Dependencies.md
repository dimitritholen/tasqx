# Dependencies

"This can't start until that is finished." Dependencies turn a pile of tasks
into an ordered plan: blocked tasks drop out of your working set, and
finishing a task tells you what it just unblocked.

## tasqx dep

Make one task wait on another. The order is: *dependent first, prerequisite
second*.

```console
tasqx dep 2 1     # task 2 waits on task 1
```

Task 2 is now **blocked**: it disappears from `tasqx list`'s default working
set and from `tasqx next`, because you can't act on it yet. It reappears the
moment task 1 is done or cancelled — and `tasqx done 1` names it in the
answer.

Blocked work isn't invisible, though: the
[dashboard](Dashboard-and-Live-View.md#tasqx-dashboard) has a BLOCKED panel
precisely because the default filter hides these tasks everywhere else.

## tasqx undep

Remove a dependency edge, same argument order as `dep`:

```console
tasqx undep 2 1   # task 2 no longer waits on task 1
```

If that was the task's last unfinished prerequisite, it unblocks immediately.

## Building a chain

Dependencies pay off when you break a feature into steps:

```console
tasqx add Design the schema
tasqx add Write the migration
tasqx add Ship it
tasqx dep 2 1     # migration waits on schema
tasqx dep 3 2     # shipping waits on migration
```

Now `tasqx next` walks you through the chain in order, one actionable task at
a time — and an AI agent working through
[MCP](AI-Agents-and-Automation.md) does exactly the same.
