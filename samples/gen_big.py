"""Generate samples/big.md (~750 KB, ~2,700 blocks) for benchmarking:
    python3 samples/gen_big.py && folio --bench samples/big.md"""
import random
random.seed(1)
words = ("the quick brown fox jumps over lazy dog markdown render layout engine pango cairo "
         "glyph shaping text block virtual scroll cache estimate measure").split()
def sent(n): return " ".join(random.choice(words) for _ in range(n)).capitalize() + "."
out = []
for sec in range(400):
    out.append(f"## Section {sec}: {sent(4)[:-1]}\n")
    for _ in range(4):
        out.append(" ".join(sent(random.randint(8, 20)) for _ in range(random.randint(2, 6)))
                   + " Some **bold**, *italic*, `code` and [a link](https://example.com).\n")
    if sec % 3 == 0:
        out.append("```rust\nfn main() {\n    let x = vec![1, 2, 3];\n    for i in &x { println!(\"{i}\"); }\n}\n```\n")
    if sec % 4 == 0:
        out.append("- item one\n- item two with more words\n  - nested\n- [ ] task\n")
    if sec % 5 == 0:
        out.append("| a | b | c |\n|---|---|---|\n"
                   + "".join(f"| {sent(2)} | {sent(3)} | {random.randint(0, 999)} |\n" for _ in range(6)))
    if sec % 7 == 0:
        out.append("> " + sent(15) + "\n")
open(__file__.replace("gen_big.py", "big.md"), "w").write("\n".join(out))
