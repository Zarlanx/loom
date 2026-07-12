# loom-ckpt

The checkpoint helper: a small **Python package** shipped inside the `train` image —
deliberately not a Cargo crate ([build README §6](../../docs/build/README.md)).
HF Trainer callback, checkpoint-now-on-eject, incremental upload, exact-step/RNG restore.
Lands with **PR-17** (`checkpoint-resume`).
