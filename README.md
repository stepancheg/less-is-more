# less-is-more

This is patter, a watch-like refreshing pager. It is an alternative
to [viddy](https://github.com/sachaos/viddy) and
[sasqwatch](https://github.com/fabio42/sasqwatch).

## Use

One use is to keep it open beside an agent and let it refresh the
diff while the agent edits. The view updates in place, so you can
watch the patch grow without rerunning the command yourself.

The refresh interval is `-n` / `--interval` in seconds. If that flag
is omitted, `LESS_IS_MORE_INTERVAL` is used. The default is 2.

```
less-is-more -n 0.5 -- sl --config ui.color=always diff
```

```
less-is-more -n 0.5 -- git -c color.ui=always diff
```

## sasqwatch

sasqwatch does not handle colors correctly and does not have a
wrapping mode.

## viddy

viddy uses ratatui and has a scrollbar that cannot be turned off,
so it is not possible to double-click to copy wrapping text.
