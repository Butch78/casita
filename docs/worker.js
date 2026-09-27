import {
  createGitHubMetadataHandler,
  createMarkdownMiddleware,
} from '@cachix/site-kit/cloudflare';

const githubTtl = 3600;
const githubMetadata = createGitHubMetadataHandler({
  repository: 'cachix/casita',
  ttl: githubTtl,
  fetch: async (url, options) => {
    const requestOptions = {
      ...options,
      cf: {
        cacheEverything: true,
        cacheTtlByStatus: { '200-299': githubTtl, '400-599': 0 },
      },
    };
    const response = await fetch(url, requestOptions);
    if (response.status === 404 && new URL(url).pathname === '/repos/cachix/casita') {
      // A 404 cached while the repository was private must be revalidated.
      return fetch(url, { ...requestOptions, cache: 'no-cache' });
    }
    return response;
  },
});
const markdown = createMarkdownMiddleware();

export default {
  async fetch(request, env) {
    if (new URL(request.url).pathname === '/api/github') {
      if (request.method !== 'GET') {
        return new Response('Method Not Allowed', {
          status: 405,
          headers: { Allow: 'GET' },
        });
      }
      return githubMetadata({ env });
    }

    return markdown({
      request,
      env,
      next: () => env.ASSETS.fetch(request),
    });
  },
};
