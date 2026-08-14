import { GlobalRegistrator } from '@happy-dom/global-registrator'

GlobalRegistrator.register()

/**
 * happy-dom starts on `about:blank`, where `pushState` and `replaceState` are
 * rejected because there is no origin to be same-origin with. The router tests
 * need a real one; nothing else reads `location`, so setting it here costs the
 * other suites nothing.
 */
declare const happyDOM: { setURL(url: string): void }
happyDOM.setURL('http://localhost/')
