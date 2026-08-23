#!/usr/bin/env python3
"""A many-small-files corpus: the shape REWRITE.md §6 sets the -j8 target on."""
import os, random, sys
root = sys.argv[1]
random.seed(20260822)
if os.path.exists(root):
    sys.exit("refusing to overwrite " + root)
FILES, DIRS = 50_000, 250
os.makedirs(root)
for d in range(DIRS):
    os.makedirs(os.path.join(root, "d{:03d}".format(d)))
# A third of the content is shared, so the stage has real groups to prove and
# real strangers to reject, the way a photo library does.
distinct = [bytes(random.getrandbits(8) for _ in range(random.randint(512, 4096)))
            for _ in range(FILES // 3)]
written = 0
for i in range(FILES):
    blob = distinct[random.randrange(len(distinct))] if i % 3 else \
           bytes(random.getrandbits(8) for _ in range(random.randint(512, 4096)))
    path = os.path.join(root, "d{:03d}".format(i % DIRS), "f{:06d}.bin".format(i))
    with open(path, "wb") as fh:
        fh.write(blob)
    written += len(blob)
print("{} files, {:.1f} MB".format(FILES, written / 1e6))
