import { createGitHubMetadataHandler } from '@cachix/site-kit/cloudflare';

export const onRequestGet = createGitHubMetadataHandler({
  repository: 'cachix/casita',
});
