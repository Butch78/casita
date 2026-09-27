// Schematic chunk recipes; the filenames come from the verified build fixture.
// Keep the executable centered while introducing the library first.
export const artifactWrites = [
  { chunks: ['A', 'B', 'C'], start: 3600 },
  { chunks: ['A', 'C', 'D'], start: 300 },
  { chunks: ['E'], start: 6900 },
];
export const writeTravel = 1400;
export const writeStagger = 300;
export const bytesReady = (artifact: typeof artifactWrites[number]) =>
  artifact.start + (artifact.chunks.length - 1) * writeStagger + writeTravel + 300;
