# RNNoise ONNX Model Source

`weights.rnn` is the quantized model shipped by `nnnoiseless` 0.5.2 and is
redistributed under its BSD-3-Clause license; see `LICENSE.nnnoiseless`.

Generate the neural-stage ONNX model and deterministic equivalence fixtures:

```bash
python3 scripts/generate-rnnoise-onnx.py --verify
```

The model contains only the recurrent neural stage. Feature extraction, pitch
filtering, gain interpolation, and synthesis remain native DSP. Inputs and
outputs include every recurrent state so a provider result can be committed at
a block boundary or discarded without corrupting the CPU fallback state.
