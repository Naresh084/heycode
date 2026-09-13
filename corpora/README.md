# Synthetic fuzz corpora

These seeds are minimal, hand-authored protocol examples for the isolated
targets in `fuzz/`. They contain no captured user/provider content, host paths,
account identifiers, credentials, or secret-shaped placeholders.

Corpus files are inputs, not evidence of a continuous run. The wrapper copies
them into an ephemeral writable corpus and keeps crash artifacts beside that
copy; neither generated inputs nor crashes are retained or uploaded by the
dedicated workflow.
