/**
 * Re-encodes the README screen recordings in ../assets into web-sized MP4s plus
 * poster frames. The originals are ~338 MB of high-bitrate 1440p/2228p capture —
 * far too heavy to serve, and far more resolution than terminal text needs.
 *
 * `bun run media posters` re-cuts the posters from the existing encodes, which
 * takes seconds; a full run re-encodes every video, which takes many minutes.
 */
import { mkdir, readdir } from 'node:fs/promises'

const here = (path: string) => new URL(`../${path}`, import.meta.url).pathname

const SRC = here('../assets')
const OUT = here('web/public/video')

interface Clip {
  name: string
  /**
   * CRF and width are per clip: the defaults land most under 5 MB, but the two
   * longest ones (adversarial-review, orchestrate) and the 3568px-wide captures
   * (discover, gossip-join, gossip-msg) need their own to stay inside the
   * page's byte budget.
   */
  crf: number
  width: number
  /** Seconds into the clip. See `poster` for why this is per clip. */
  posterAt: number
}

const CLIPS: Clip[] = [
  { name: 'readme-demo', crf: 30, width: 1440, posterAt: 99 },
  { name: 'readme-create-join', crf: 30, width: 1440, posterAt: 30 },
  { name: 'readme-gossip-join', crf: 32, width: 1440, posterAt: 41 },
  { name: 'readme-topic', crf: 30, width: 1440, posterAt: 62 },
  { name: 'readme-gossip-msg', crf: 32, width: 1440, posterAt: 60 },
  { name: 'readme-task', crf: 30, width: 1440, posterAt: 73 },
  { name: 'readme-adversarial-review', crf: 32, width: 1440, posterAt: 136 },
  { name: 'readme-orchestrate', crf: 30, width: 1152, posterAt: 145 },
  { name: 'readme-discover', crf: 32, width: 1440, posterAt: 55 },
]

async function ffmpeg(args: string[]) {
  const proc = Bun.spawn(['ffmpeg', '-y', '-loglevel', 'error', ...args], {
    stdout: 'inherit',
    stderr: 'inherit',
  })
  if ((await proc.exited) !== 0) {
    console.error(`\nffmpeg ${args.join(' ')} failed`)
    process.exit(1)
  }
}

const mb = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(1)}M`

async function encode({ name, crf, width }: Clip) {
  await ffmpeg([
    '-i',
    `${SRC}/${name}.mp4`,
    '-vf',
    `scale='min(${width},iw)':-2:flags=lanczos`,
    '-c:v',
    'libx264',
    '-preset',
    'slow',
    '-crf',
    String(crf),
    '-tune',
    'stillimage',
    '-pix_fmt',
    'yuv420p',
    '-movflags',
    '+faststart',
    '-an',
    `${OUT}/${name}.mp4`,
  ])
  console.log(`${name.padEnd(32)} ${mb(Bun.file(`${OUT}/${name}.mp4`).size)}`)
}

/**
 * The poster is the click target on the landing page, and it is pulled from the
 * encode rather than the source so it matches the first frame after play. The
 * timestamp is per clip: a fixed early offset lands on a freshly cleared screen
 * for most of these, which is a poster of nothing.
 *
 * `-ss` stays ahead of `-i` — as an input option it seeks rather than decoding
 * up to the timestamp, which is the difference between instant and a minute.
 */
async function poster({ name, posterAt }: Clip) {
  const at = String(posterAt)
  const common = ['-ss', at, '-i', `${OUT}/${name}.mp4`, '-frames:v', '1', '-q:v', '4']

  await ffmpeg([...common, `${OUT}/${name}.jpg`])
  await ffmpeg([...common, '-vf', 'scale=720:-2', `${OUT}/${name}@720.jpg`])
}

const postersOnly = process.argv[2] === 'posters'

await mkdir(OUT, { recursive: true })

for (const clip of CLIPS) {
  if (!postersOnly) await encode(clip)
  await poster(clip)
}

const total = (await readdir(OUT)).reduce((sum, file) => sum + Bun.file(`${OUT}/${file}`).size, 0)
console.log(`---\n${mb(total)}\t${OUT}`)
