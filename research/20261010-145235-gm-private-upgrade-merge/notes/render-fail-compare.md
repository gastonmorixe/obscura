# Render fail compare

Source: upgrade branch j8 vs upstream/main worktree j13.

Result: **IDENTICAL 29 failures** on both tips.

Decision: defer. Not merge regressions. Pre-existing on this macOS host (font/geometry metrics).

Merge-related fails fixed separately (CDP registry, stealth UA, AX mod tests, navigation timing terminate).
