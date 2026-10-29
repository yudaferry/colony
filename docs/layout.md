# Spawn layout

`colony spawn` (and `colony split`) use a fixed default layout so the window stays tidy.

```
+------------------+------------------+
|                  |  first spawned   |
|                  +------------------+
|   lead (left     |  second spawned  |
|   50%, full      +------------------+
|   height)        |  third spawned   |
+------------------+------------------+
```

- The pane that runs `spawn` (the lead) keeps the **left 50%** of the window.
- The **first** spawned pane splits the lead to the right at 50% width. It starts the right column.
- **Later** panes are added below the last pane of that column, and the column is then resized so all its panes have **equal heights**.
- `--direction right|down` skips all of this and does a plain split of the current pane.

## Which panes form the column

Each named spawned pane records the pane that spawned it (`spawned_by` in `names.json`). The column is the live panes spawned by the current pane, in creation order. Panes that were closed are dropped, so the column re-balances after a close. Spawns without a name are not tracked.

## herdr

- First pane: `herdr pane split --pane <lead> --direction right --ratio 0.5 --no-focus`. In herdr, `--ratio` is the share **kept by the old pane**, so 0.5 gives each side half.
- Next panes: split the last column pane downwards with `--ratio 0.5`, then read the layout (`herdr pane layout`) and call `herdr pane resize` to even out the heights. For three panes, for example, the upper panes are moved so each gets a third. A divider that is off by less than one row is left alone.
- Resizing is best effort: if it fails, the new pane already exists and the spawn goes on.

## tmux

- First pane: `split-window -h -d -t <lead> -l 50%`.
- Next panes: `split-window -v -d -t <last column pane>`, then `select-layout -E` on the new pane, which spreads it and its vertical siblings evenly without touching the lead.
- The `select-layout` step is best effort too.

New panes are created without taking focus, so the lead stays where you are typing.
