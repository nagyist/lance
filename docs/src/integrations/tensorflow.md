# Tensorflow Integration

Lance can be used as a regular [tf.data.Dataset](https://www.tensorflow.org/api_docs/python/tf/data/Dataset)
in [Tensorflow](https://www.tensorflow.org/).

!!! warning

    This feature is experimental and the APIs may change in the future.

## Reading from Lance

Using `lance.tf.data.from_lance`, you can create an `tf.data.Dataset` easily.

```python
import tensorflow as tf
import lance

# Create tf dataset
ds = lance.tf.data.from_lance("s3://my-bucket/my-dataset")

# Chain tf dataset with other tf primitives

for batch in ds.shuffling(32).map(lambda x: tf.io.decode_png(x["image"])):
    print(batch)
```

Backed by the Lance [columnar format](../format/index.md), using `lance.tf.data.from_lance` supports
efficient column selection, filtering, and more.

```python
ds = lance.tf.data.from_lance(
    "s3://my-bucket/my-dataset",
    columns=["image", "label"],
    filter="split = 'train' AND collected_time > timestamp '2020-01-01'",
    batch_size=256)
```

By default, Lance will infer the Tensor spec from the projected columns. You can also specify `tf.TensorSpec` manually.

```python
batch_size = 256
ds = lance.tf.data.from_lance(
    "s3://my-bucket/my-dataset",
    columns=["image", "labels"],
    batch_size=batch_size,
    output_signature={
        "image": tf.TensorSpec(shape=(), dtype=tf.string),
        "labels": tf.RaggedTensorSpec(
            dtype=tf.int32, shape=(batch_size, None), ragged_rank=1),
    },
```

## Distributed Training and Shuffling

Since [a Lance Dataset is a set of Fragments](../format/index.md), we can distribute and shuffle Fragments to different
workers.

```python
import tensorflow as tf
from lance.tf.data import from_lance, lance_fragments

world_size = 32
rank = 10
seed = 123  #
epoch = 100

dataset_uri = "s3://my-bucket/my-dataset"

# Shuffle fragments distributedly.
fragments =
    lance_fragments("s3://my-bucket/my-dataset")
    .shuffling(32, seed=seed)
    .repeat(epoch)
    .enumerate()
    .filter(lambda i, _: i % world_size == rank)
    .map(lambda _, fid: fid)

ds = from_lance(
    uri,
    columns=["image", "label"],
    fragments=fragments,
    batch_size=32
    )
for batch in ds:
    print(batch)
```

!!! warning

    For multiprocessing you should probably not use fork as lance is
    multi-threaded internally and fork and multi-thread do not work well.
    Refer to [this discussion](https://discuss.python.org/t/concerns-regarding-deprecation-of-fork-with-alive-threads/33555). 