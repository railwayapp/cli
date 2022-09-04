/**
 * Global configuration
 */
export const CONFIG = {
  /**
   * The name of the binary
   * @type {string}
   */
  name: "railway",

  /**
   * Where to save the unzipped files
   * @type {string}
   */
  path: "./bin",

  /**
   * The URL to download the binary form
   *
   * - `{{arch}}` is one of the Golang achitectures listed below
   * - `{{bin_name}}` is the name declared above
   * - `{{platform}}` is one of the Golang platforms listed below
   * - `{{version}}` is the version number as `0.0.0` (taken from package.json)
   *
   * @type {string}
   */
  url: "https://github.com/railwayapp/cli/releases/download/v{{version}}/{{bin_name}}_{{version}}_{{platform}}_{{arch}}.tar.gz",
};

/**
 * Mapping from Node's `process.arch` to Golang's `$GOARCH`
 */
export const ARCH_MAPPING = {
  ia32: "386",
  x64: "amd64",
  arm64: "arm64",
};

/**
 * Mapping between Node's `process.platform` to Golang's
 */
export const PLATFORM_MAPPING = {
  darwin: "darwin",
  linux: "linux",
  win32: "windows",
  // freebsd: "freebsd",
};
