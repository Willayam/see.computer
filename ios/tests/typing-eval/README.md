# Typing eval sentences

Held-out English and Swedish sentences that `scripts/typing-sim.sh` types with simulated finger noise. `dev-*` is for tuning, and `test-*` is only for confirming a result at the end.

The sentences come from [Tatoeba](https://tatoeba.org) under [CC BY 2.0 FR](https://creativecommons.org/licenses/by/2.0/fr/). `scripts/build-typing-eval.py` samples them by hash and drops any sentence the keyboard's word tables learn from.
