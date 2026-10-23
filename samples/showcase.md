---
title: Folio showcase
tags: [markdown, demo]
---

# Folio — a fast Markdown viewer

Folio renders **CommonMark** and *GitHub Flavored Markdown* with ~~heavy~~ zero runtime overhead. It supports `inline code`, [links](https://example.com), <kbd>Ctrl</kbd>+<kbd>O</kbd>, H<sub>2</sub>O, E = mc<sup>2</sup>, and inline math like $e^{i\pi} + 1 = 0$ or $\sum_{k=1}^{n} k = \frac{n(n+1)}{2}$.

## Lists

- First item with some text that is long enough to wrap onto a second line so we can check the hanging indent looks right
- Second item
  - Nested item
    - Deeper item
- Third item

1. Ordered one
2. Ordered two
3. Ordered three

- [x] Write the parser
- [x] Write the layout engine
- [ ] Ship the editor

## Code

```rust
/// Lay out every block intersecting the viewport.
pub fn ensure(&mut self, top: f64, bottom: f64) {
    let mut i = self.block_at(top);
    while i < self.len() && self.ys[i] < bottom {
        self.ensure_block(i); // 42 µs on average
        i += 1;
    }
}
```

```python
def fib(n: int) -> int:
    """Return the n-th Fibonacci number."""
    return n if n < 2 else fib(n - 1) + fib(n - 2)
```

## Quotes and callouts

> Simplicity is prerequisite for reliability.
>
> — Edsger W. Dijkstra

> [!NOTE]
> Folio never runs a web engine. Everything is drawn natively.

> [!WARNING]
> Remote images are shown as links, not fetched.

## Table

| Action | Shortcut | Notes |
|:-------|:--------:|------:|
| Open file | Ctrl+O | or drag a file onto the window |
| Find | Ctrl+F | Enter / Shift+Enter to step |
| Zoom | Ctrl+= / Ctrl+− | Ctrl+scroll works too |
| Dark mode | Ctrl+D | follows the system by default |

## Math

$$
\int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}
$$

## Image

![A gradient](gradient.png)

Remote badge: ![build passing](https://img.shields.io/badge/build-passing-green)

---

That's all. Here is a footnote reference[^1] and another[^why].

[^1]: Footnotes are collected at the end of the document.
[^why]: Because GitHub does it that way.
