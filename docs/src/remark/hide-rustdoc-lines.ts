import { defineMdastPlugin } from 'satteri';

/** Keep Rustdoc's compile-only setup out of rendered examples. */
export const hideRustdocLines = defineMdastPlugin({
  name: 'hide-rustdoc-lines',
  code(node) {
    if (node.lang !== 'rust') return;
    return {
      ...node,
      value: node.value
        .split('\n')
        .filter((line) => !/^#(?: |$)/.test(line))
        .join('\n'),
    };
  },
});
