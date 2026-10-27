// ESM resolve hook for the pinned published provider source.
//
// `pi-devin@0.2.1` is published as TypeScript whose relative imports use the
// `.js` extension (`./context-map.js`). Node's type stripping executes `.ts`
// files but does not remap `.js` specifiers, so this hook maps a `.js`
// specifier that only exists as `.ts` inside the pinned SDK root. It never
// resolves outside the root, never maps bare specifiers and never loads
// anything the module manifest did not already hash.

let sdkRoot = null;

export const initialize = data => {
  if (!data || typeof data.sdkRoot !== 'string' || !data.sdkRoot.startsWith('/')) {
    throw new Error('sdk root is missing');
  }
  sdkRoot = data.sdkRoot.replace(/\/$/, '');
};

export const resolve = async (specifier, context, nextResolve) => {
  try {
    return await nextResolve(specifier, context);
  } catch (error) {
    if (!sdkRoot || !specifier.startsWith('.') || !specifier.endsWith('.js')) throw error;
    const parent = context?.parentURL;
    if (typeof parent !== 'string' || !parent.startsWith('file://')) throw error;
    const parentPath = new URL(parent).pathname;
    if (!parentPath.startsWith(sdkRoot + '/')) throw error;
    const candidate = new URL(specifier.slice(0, -3) + '.ts', parent);
    if (!candidate.pathname.startsWith(sdkRoot + '/')) throw error;
    return { url: candidate.href, shortCircuit: true };
  }
};
