'use client'
import { useRef, useState } from 'react'

import { PlayIcon } from './icons'

export interface Demo {
  id: string
  title: string
  caption: string
  /** Basename under /video/, without extension. */
  clip: string
  duration: string
  docs?: string
}

/** A phone cannot show a 160-column capture; fullscreen is the only readable size. */
const PHONE = 640

interface Props {
  demos: Demo[]
  /** A single-clip player drops the tab rail. */
  showTabs?: boolean
}

export function DemoPlayer({ demos, showTabs = true }: Props) {
  const [active, setActive] = useState(0)
  const [playing, setPlaying] = useState(false)
  const videoRef = useRef<HTMLVideoElement>(null)
  const frameRef = useRef<HTMLDivElement>(null)
  const tabsRef = useRef<HTMLDivElement>(null)

  const demo = demos[active]!

  // The <video> carries no src until this runs, so the page loads no video
  // bytes — `preload="none"` alone still lets Safari probe for metadata.
  function play() {
    const video = videoRef.current
    if (!video) return
    video.src = `/video/${demo.clip}.mp4`
    video.load()
    setPlaying(true)
    void video.play()
    if (window.innerWidth < PHONE) {
      const native = video as HTMLVideoElement & { webkitEnterFullscreen?: () => void }
      if (native.webkitEnterFullscreen) native.webkitEnterFullscreen()
      else void frameRef.current?.requestFullscreen?.()
    }
  }

  function select(index: number) {
    const video = videoRef.current
    if (video) {
      video.pause()
      video.removeAttribute('src')
      video.load()
    }
    setPlaying(false)
    setActive(index)
  }

  function onTabKey(event: React.KeyboardEvent) {
    const next =
      event.key === 'ArrowDown' || event.key === 'ArrowRight'
        ? (active + 1) % demos.length
        : event.key === 'ArrowUp' || event.key === 'ArrowLeft'
          ? (active - 1 + demos.length) % demos.length
          : event.key === 'Home'
            ? 0
            : event.key === 'End'
              ? demos.length - 1
              : -1
    if (next < 0) return
    event.preventDefault()
    select(next)
    tabsRef.current?.querySelectorAll('button')[next]?.focus()
  }

  return (
    <div className={showTabs ? 'demo demo-tabbed' : 'demo'}>
      {showTabs && (
        <div className="demo-rail" role="tablist" aria-label="Command demos" ref={tabsRef}>
          {demos.map((item, index) => (
            <button
              key={item.id}
              type="button"
              role="tab"
              id={`demo-tab-${item.id}`}
              aria-selected={index === active}
              aria-controls="demo-frame"
              tabIndex={index === active ? 0 : -1}
              className="demo-tab"
              onKeyDown={onTabKey}
              onClick={() => select(index)}
            >
              {item.title}
            </button>
          ))}
        </div>
      )}

      <div className="demo-main">
        <div
          className="demo-frame"
          id="demo-frame"
          role="tabpanel"
          aria-labelledby={showTabs ? `demo-tab-${demo.id}` : undefined}
          ref={frameRef}
        >
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
            className="demo-video"
          />
          {!playing && (
            <button type="button" className="demo-poster" onClick={play}>
              <img
                src={`/video/${demo.clip}.jpg`}
                srcSet={`/video/${demo.clip}@720.jpg 720w, /video/${demo.clip}.jpg 1440w`}
                sizes="(min-width: 1024px) 1000px, 100vw"
                width={1440}
                height={900}
                alt=""
                decoding="async"
              />
              <span className="demo-play">
                <PlayIcon />
                <span className="sr-only">
                  Play the {demo.title} demo, {demo.duration}, no audio
                </span>
              </span>
            </button>
          )}
        </div>

        <p className="demo-caption">
          {demo.caption}{' '}
          <span className="demo-meta">
            {demo.duration} · no audio
            {demo.docs ? (
              <>
                {' · '}
                <a href={demo.docs}>docs →</a>
              </>
            ) : null}
          </span>
        </p>
      </div>
    </div>
  )
}
