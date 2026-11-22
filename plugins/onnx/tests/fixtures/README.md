# Test fixtures

## `add_f32.onnx`

A 123-byte model: two f32 inputs `a` and `b`, both of shape `[1, 3]`, and one output
`sum = a + b` (a single `Add` node). Opset 13 (default domain), IR version 8, producer name
`frust-onnx-fixtures`. SHA-256 `46e92a6c454d9b8a5b4195480d72d4190eb07e27caf06201e5a9057d616555cc`.

Generated with Python 3.14.7 and the `onnx` package 1.23.2 (pulling protobuf 7.36.2, numpy 2.5.5,
ml_dtypes 0.7.0 and typing_extensions 4.16.0), installed in a throwaway virtualenv, never the
system Python:

```sh
python3 -m venv venv
venv/bin/pip install onnx
venv/bin/python -I make_add_f32.py add_f32.onnx
```

`make_add_f32.py`:

```python
import sys

import onnx
from onnx import TensorProto, helper

a = helper.make_tensor_value_info("a", TensorProto.FLOAT, [1, 3])
b = helper.make_tensor_value_info("b", TensorProto.FLOAT, [1, 3])
total = helper.make_tensor_value_info("sum", TensorProto.FLOAT, [1, 3])
node = helper.make_node("Add", ["a", "b"], ["sum"])
graph = helper.make_graph([node], "add_f32", [a, b], [total])
model = helper.make_model(
    graph,
    producer_name="frust-onnx-fixtures",
    opset_imports=[helper.make_opsetid("", 13)],
)
model.ir_version = 8
onnx.checker.check_model(model, full_check=True)
onnx.save(model, sys.argv[1])
```

The output is byte-for-byte reproducible with these versions. IR version 8 and opset 13 are set
explicitly, rather than taking the `onnx` package's newest defaults, so the model also loads on
ONNX Runtime releases older than the pinned one.
