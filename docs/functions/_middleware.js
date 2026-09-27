import { createMarkdownMiddleware } from '@cachix/site-kit/cloudflare';

export const onRequest = [createMarkdownMiddleware()];
