import { createBasicAuthMiddleware, createMarkdownMiddleware } from '@cachix/site-kit/cloudflare';

export const onRequest = [
  createBasicAuthMiddleware({
    realm: 'casita preview',
  }),
  createMarkdownMiddleware(),
];
