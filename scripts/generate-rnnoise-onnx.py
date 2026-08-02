#!/usr/bin/env python3
"""Reproducibly convert nnnoiseless' quantized RNNoise RNN to ONNX.

This intentionally converts only the neural stage. Every recurrent state is an
explicit model input and output, which lets WaveLinux commit a provider result
only when it arrives before its deadline. The native CPU state remains the
authoritative fallback for a missed or failed provider block.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

import numpy as np


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_WEIGHTS = ROOT / "providers" / "rnnoise" / "weights.rnn"
DEFAULT_MODEL = ROOT / "providers" / "rnnoise" / "rnnoise-neural-v1.onnx"
DEFAULT_FIXTURE = ROOT / "providers" / "rnnoise" / "rnnoise-neural-v1-golden.json"
MODEL_VERSION = 1
OPSET_VERSION = 17
MAX_ABS_ERROR = 1.0e-4


@dataclass(frozen=True)
class Dense:
    inputs: int
    neurons: int
    activation: int
    weights: np.ndarray
    bias: np.ndarray


@dataclass(frozen=True)
class Gru:
    inputs: int
    neurons: int
    activation: int
    weights: np.ndarray
    recurrent: np.ndarray
    bias: np.ndarray


@dataclass(frozen=True)
class Model:
    input_dense: Dense
    vad_gru: Gru
    noise_gru: Gru
    denoise_gru: Gru
    denoise_output: Dense
    vad_output: Dense


def parse_weights(path: Path) -> Model:
    data = np.frombuffer(path.read_bytes(), dtype=np.int8)
    offset = 0

    def header() -> tuple[int, int, int]:
        nonlocal offset
        if offset + 3 > data.size:
            raise ValueError("truncated RNNoise model header")
        inputs, neurons, activation = (int(value) for value in data[offset : offset + 3])
        offset += 3
        if inputs <= 0 or neurons <= 0 or activation not in (0, 1, 2):
            raise ValueError("invalid RNNoise layer header")
        return inputs, neurons, activation

    def array(length: int) -> np.ndarray:
        nonlocal offset
        if offset + length > data.size:
            raise ValueError("truncated RNNoise model weights")
        value = data[offset : offset + length].astype(np.float32, copy=True)
        offset += length
        return value

    def dense() -> Dense:
        inputs, neurons, activation = header()
        weights = array(inputs * neurons).reshape(inputs, neurons)
        bias = array(neurons)
        return Dense(inputs, neurons, activation, weights, bias)

    def gru() -> Gru:
        inputs, neurons, activation = header()
        weights = array(3 * inputs * neurons).reshape(inputs, 3 * neurons)
        recurrent = array(3 * neurons * neurons).reshape(neurons, 3 * neurons)
        bias = array(3 * neurons)
        return Gru(inputs, neurons, activation, weights, recurrent, bias)

    model = Model(dense(), gru(), gru(), gru(), dense(), dense())
    if offset != data.size:
        raise ValueError(f"RNNoise model has {data.size - offset} trailing bytes")
    if (
        model.input_dense.inputs != 42
        or model.denoise_output.neurons != 22
        or model.vad_output.neurons != 1
    ):
        raise ValueError("RNNoise model dimensions do not match protocol v1")
    return model


def tansig_approx(value: np.ndarray) -> np.ndarray:
    """Vector form of nnnoiseless' table-interpolated tanh approximation."""
    clipped = np.clip(value, -8.0, 8.0)
    absolute = np.abs(clipped)
    index = np.floor(0.5 + 25.0 * absolute).astype(np.int64)
    index = np.clip(index, 0, 200)
    table = np.array([round(math.tanh(i * 0.04), 6) for i in range(201)], dtype=np.float32)
    remainder = absolute - np.float32(0.04) * index.astype(np.float32)
    base = table[index]
    derivative = np.float32(1.0) - base * base
    result = base + remainder * derivative * (np.float32(1.0) - base * remainder)
    result = np.copysign(result, clipped)
    result = np.where(value >= 8.0, np.float32(1.0), result)
    return np.where(value <= -8.0, np.float32(-1.0), result).astype(np.float32)


def activation(function: int, value: np.ndarray) -> np.ndarray:
    if function == 0:
        return tansig_approx(value)
    if function == 1:
        return np.float32(0.5) + np.float32(0.5) * tansig_approx(np.float32(0.5) * value)
    if function == 2:
        return np.maximum(value, np.float32(0.0))
    raise ValueError(f"unsupported activation {function}")


def dense_reference(layer: Dense, value: np.ndarray) -> np.ndarray:
    result = layer.bias + value @ layer.weights
    return activation(layer.activation, result * np.float32(1.0 / 256.0))


def gru_reference(layer: Gru, value: np.ndarray, state: np.ndarray) -> np.ndarray:
    n = layer.neurons
    scale = np.float32(1.0 / 256.0)
    z = activation(
        1,
        (layer.bias[:n] + value @ layer.weights[:, :n] + state @ layer.recurrent[:, :n])
        * scale,
    )
    reset = state * activation(
        1,
        (
            layer.bias[n : 2 * n]
            + value @ layer.weights[:, n : 2 * n]
            + state @ layer.recurrent[:, n : 2 * n]
        )
        * scale,
    )
    candidate = activation(
        layer.activation,
        (
            layer.bias[2 * n :]
            + value @ layer.weights[:, 2 * n :]
            + reset @ layer.recurrent[:, 2 * n :]
        )
        * scale,
    )
    return (z * state + (np.float32(1.0) - z) * candidate).astype(np.float32)


def reference_step(
    model: Model,
    features: np.ndarray,
    vad_state: np.ndarray,
    noise_state: np.ndarray,
    denoise_state: np.ndarray,
) -> tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray, np.ndarray]:
    dense = dense_reference(model.input_dense, features)
    vad_state = gru_reference(model.vad_gru, dense, vad_state)
    vad = dense_reference(model.vad_output, vad_state)
    noise_input = np.concatenate((dense, vad_state, features))
    noise_state = gru_reference(model.noise_gru, noise_input, noise_state)
    denoise_input = np.concatenate((vad_state, noise_state, features))
    denoise_state = gru_reference(model.denoise_gru, denoise_input, denoise_state)
    gains = dense_reference(model.denoise_output, denoise_state)
    return gains, vad, vad_state, noise_state, denoise_state


def generate_onnx(model: Model, destination: Path) -> bytes:
    try:
        import onnx
        from onnx import TensorProto, helper, numpy_helper
    except ImportError as error:
        raise SystemExit("python package 'onnx' is required to generate the model") from error

    nodes = []
    initializers = []

    def tensor(name: str, value: np.ndarray) -> str:
        initializers.append(numpy_helper.from_array(value.astype(np.float32), name=name))
        return name

    one = tensor("constant_one", np.array([1.0], dtype=np.float32))

    def activate(prefix: str, function: int, source: str) -> str:
        output = f"{prefix}_activation"
        operation = {0: "Tanh", 1: "Sigmoid", 2: "Relu"}[function]
        nodes.append(helper.make_node(operation, [source], [output], name=f"{prefix}_{operation.lower()}"))
        return output

    def dense(prefix: str, source: str, layer: Dense) -> str:
        weight = tensor(f"{prefix}_weight", layer.weights / np.float32(256.0))
        bias = tensor(f"{prefix}_bias", layer.bias / np.float32(256.0))
        multiplied = f"{prefix}_matmul"
        biased = f"{prefix}_biased"
        nodes.append(helper.make_node("MatMul", [source, weight], [multiplied], name=multiplied))
        nodes.append(helper.make_node("Add", [multiplied, bias], [biased], name=biased))
        return activate(prefix, layer.activation, biased)

    def gru(prefix: str, source: str, state: str, layer: Gru) -> str:
        n = layer.neurons

        def gate(label: str, gate_index: int, recurrent_source: str, function: int) -> str:
            start = gate_index * n
            end = start + n
            weight = tensor(
                f"{prefix}_{label}_weight",
                layer.weights[:, start:end] / np.float32(256.0),
            )
            recurrent = tensor(
                f"{prefix}_{label}_recurrent",
                layer.recurrent[:, start:end] / np.float32(256.0),
            )
            bias = tensor(
                f"{prefix}_{label}_bias",
                layer.bias[start:end] / np.float32(256.0),
            )
            input_product = f"{prefix}_{label}_input"
            recurrent_product = f"{prefix}_{label}_recurrent_product"
            combined = f"{prefix}_{label}_combined"
            biased = f"{prefix}_{label}_biased"
            nodes.append(helper.make_node("MatMul", [source, weight], [input_product], name=input_product))
            nodes.append(
                helper.make_node(
                    "MatMul",
                    [recurrent_source, recurrent],
                    [recurrent_product],
                    name=recurrent_product,
                )
            )
            nodes.append(helper.make_node("Add", [input_product, recurrent_product], [combined], name=combined))
            nodes.append(helper.make_node("Add", [combined, bias], [biased], name=biased))
            return activate(f"{prefix}_{label}", function, biased)

        update = gate("update", 0, state, 1)
        reset_gate = gate("reset", 1, state, 1)
        reset_state = f"{prefix}_reset_state"
        nodes.append(helper.make_node("Mul", [state, reset_gate], [reset_state], name=reset_state))
        candidate = gate("candidate", 2, reset_state, layer.activation)
        retained = f"{prefix}_retained"
        inverse_update = f"{prefix}_inverse_update"
        replaced = f"{prefix}_replaced"
        output = f"{prefix}_out"
        nodes.append(helper.make_node("Mul", [update, state], [retained], name=retained))
        nodes.append(helper.make_node("Sub", [one, update], [inverse_update], name=inverse_update))
        nodes.append(helper.make_node("Mul", [inverse_update, candidate], [replaced], name=replaced))
        nodes.append(helper.make_node("Add", [retained, replaced], [output], name=output))
        return output

    input_dense = dense("input_dense", "features", model.input_dense)
    vad_state = gru("vad_gru", input_dense, "vad_state", model.vad_gru)
    vad = dense("vad_output", vad_state, model.vad_output)
    noise_input = "noise_input"
    nodes.append(
        helper.make_node(
            "Concat",
            [input_dense, vad_state, "features"],
            [noise_input],
            axis=1,
            name=noise_input,
        )
    )
    noise_state = gru("noise_gru", noise_input, "noise_state", model.noise_gru)
    denoise_input = "denoise_input"
    nodes.append(
        helper.make_node(
            "Concat",
            [vad_state, noise_state, "features"],
            [denoise_input],
            axis=1,
            name=denoise_input,
        )
    )
    denoise_state = gru(
        "denoise_gru", denoise_input, "denoise_state", model.denoise_gru
    )
    gains = dense("denoise_output", denoise_state, model.denoise_output)

    def value(name: str, width: int):
        return helper.make_tensor_value_info(name, TensorProto.FLOAT, [1, width])

    graph = helper.make_graph(
        nodes,
        "wavelinux6_rnnoise_neural_v1",
        [
            value("features", 42),
            value("vad_state", model.vad_gru.neurons),
            value("noise_state", model.noise_gru.neurons),
            value("denoise_state", model.denoise_gru.neurons),
        ],
        [
            value("gains", 22),
            value("vad_probability", 1),
            value("vad_state_out", model.vad_gru.neurons),
            value("noise_state_out", model.noise_gru.neurons),
            value("denoise_state_out", model.denoise_gru.neurons),
        ],
        initializer=initializers,
    )
    # Preserve stable public output names while keeping internal names readable.
    nodes.extend([])
    graph.node.extend(
        [
            helper.make_node("Identity", [gains], ["gains"], name="gains_output"),
            helper.make_node("Identity", [vad], ["vad_probability"], name="vad_output_value"),
            helper.make_node("Identity", [vad_state], ["vad_state_out"], name="vad_state_output"),
            helper.make_node("Identity", [noise_state], ["noise_state_out"], name="noise_state_output"),
            helper.make_node(
                "Identity", [denoise_state], ["denoise_state_out"], name="denoise_state_output"
            ),
        ]
    )
    generated = helper.make_model(
        graph,
        producer_name="wavelinux6",
        producer_version=str(MODEL_VERSION),
        opset_imports=[helper.make_opsetid("", OPSET_VERSION)],
        ir_version=9,
    )
    generated.model_version = MODEL_VERSION
    generated.doc_string = "WaveLinux 6 RNNoise neural stage; generated from nnnoiseless weights.rnn"
    onnx.checker.check_model(generated)
    payload = generated.SerializeToString(deterministic=True)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(payload)
    return payload


def generate_fixture(model: Model, weights: bytes, destination: Path) -> dict:
    rng = np.random.default_rng(0x574156454C494E55)
    cases = []
    states = [
        np.zeros(model.vad_gru.neurons, dtype=np.float32),
        np.zeros(model.noise_gru.neurons, dtype=np.float32),
        np.zeros(model.denoise_gru.neurons, dtype=np.float32),
    ]
    for index in range(12):
        features = rng.normal(0.0, 1.25, 42).astype(np.float32)
        prior = [state.copy() for state in states]
        gains, vad, *states = reference_step(model, features, *states)
        cases.append(
            {
                "index": index,
                "features": features.tolist(),
                "vad_state": prior[0].tolist(),
                "noise_state": prior[1].tolist(),
                "denoise_state": prior[2].tolist(),
                "gains": gains.tolist(),
                "vad_probability": float(vad[0]),
                "vad_state_out": states[0].tolist(),
                "noise_state_out": states[1].tolist(),
                "denoise_state_out": states[2].tolist(),
            }
        )
    fixture = {
        "schema_version": 1,
        "model_version": MODEL_VERSION,
        "weights_sha256": hashlib.sha256(weights).hexdigest(),
        "max_abs_error": MAX_ABS_ERROR,
        "cases": cases,
    }
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(fixture, indent=2, sort_keys=True) + "\n")
    return fixture


def verify(model_path: Path, fixture: dict, provider: str) -> float:
    try:
        import onnxruntime as ort
    except ImportError as error:
        raise SystemExit("python package 'onnxruntime' is required for --verify") from error

    requested = {
        "cpu": "CPUExecutionProvider",
        "cuda": "CUDAExecutionProvider",
        "openvino": "OpenVINOExecutionProvider",
        "migraphx": "MIGraphXExecutionProvider",
    }[provider]
    available = ort.get_available_providers()
    if requested not in available:
        raise SystemExit(f"requested {requested} is unavailable; available={available}")
    providers = [requested]
    if requested != "CPUExecutionProvider":
        providers.append("CPUExecutionProvider")
    session = ort.InferenceSession(str(model_path), providers=providers)
    maximum = 0.0
    output_names = [
        "gains",
        "vad_probability",
        "vad_state_out",
        "noise_state_out",
        "denoise_state_out",
    ]
    for case in fixture["cases"]:
        inputs = {
            name: np.asarray(case[name], dtype=np.float32)[None, :]
            for name in ("features", "vad_state", "noise_state", "denoise_state")
        }
        outputs = session.run(output_names, inputs)
        for name, actual in zip(output_names, outputs):
            expected = np.asarray(case[name], dtype=np.float32)
            maximum = max(maximum, float(np.max(np.abs(actual.reshape(-1) - expected))))
    if maximum > float(fixture["max_abs_error"]):
        raise SystemExit(
            f"RNNoise ONNX equivalence failed: max_abs_error={maximum:.9g} "
            f"limit={fixture['max_abs_error']}"
        )
    return maximum


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--weights", type=Path, default=DEFAULT_WEIGHTS)
    parser.add_argument("--model", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument(
        "--provider", choices=("cpu", "cuda", "openvino", "migraphx"), default="cpu"
    )
    args = parser.parse_args()

    weights = args.weights.read_bytes()
    parsed = parse_weights(args.weights)
    payload = generate_onnx(parsed, args.model)
    fixture = generate_fixture(parsed, weights, args.fixture)
    report = {
        "model": str(args.model),
        "model_sha256": hashlib.sha256(payload).hexdigest(),
        "weights_sha256": hashlib.sha256(weights).hexdigest(),
        "fixture": str(args.fixture),
        "provider": args.provider if args.verify else None,
    }
    if args.verify:
        report["max_abs_error"] = verify(args.model, fixture, args.provider)
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
