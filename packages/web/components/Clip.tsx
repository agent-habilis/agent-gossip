'use client'
import { useRef, useState } from 'react'

import { PlayIcon } from './icons'

interface Props {
  /** Basename under /video/, without extension. */
  clip: string
  title: string
  duration: string
}

/** A phone cannot show a 160-column capture; fullscreen is the only readable size. */
const PHONE = 640

export function Clip({ clip, title, duration }: Props) {
  const [playing, setPlaying] = useState(false)
  const videoRef = useRef<HTMLVideoElement>(null)

  // The <video> carries no src until this runs, so the page loads no video
  // bytes — `preload="none"` alone still lets Safari probe for metadata.
  function play() {
    const video = videoRef.current
    if (!video) return
    video.src = `/video/${clip}.mp4`
    video.load()
    setPlaying(true)
    void video.play()
    if (window.innerWidth < PHONE) {
      const native = video as HTMLVideoElement & { webkitEnterFullscreen?: () => void }
      if (native.webkitEnterFullscreen) native.webkitEnterFullscreen()
      else void video.requestFullscreen?.()
    }
  }

  return (
    <div className="clip">
      <video
        ref={videoRef}
        preload="none"
        playsInline
        // The encodes carry no audio track (`-an`), so this states a fact
        // rather than silencing anything — and it keeps play() from being
        // refused where a gesture is not credited.
        muted
        controls={playing}
        hidden={!playing}
      />
      {!playing && (
        <button type="button" className="clip-poster" onClick={play}>
          <img
            src={`/video/${clip}.jpg`}
            srcSet={`/video/${clip}@720.jpg 720w, /video/${clip}.jpg 1440w`}
            sizes="(min-width: 1024px) 800px, 100vw"
            width={1440}
            height={900}
            alt=""
            loading="lazy"
            decoding="async"
          />
          <span className="clip-play">
            <PlayIcon />
            <span className="sr-only">
              Play the {title} demo, {duration}, no audio
            </span>
          </span>
        </button>
      )}
    </div>
  )
}
