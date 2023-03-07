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
	url: "https://github.com/railwayapp/cli/releases/download/v{{version}}/{{bin_name}}-v{{version}}-{{triple}}.tar.gz",
};
