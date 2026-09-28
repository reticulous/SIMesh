/* The board every node is, as testbed/boards.py has it: an SX1262.
 *
 * A node's maximum power at the antenna connector is its own `max_dbm`, or
 * the chip's 22 dBm when it states none. Above 22 dBm the node has a GC1109
 * front-end module, as a Heltec V4 does, which reaches 27 dBm. What a node
 * sends when nothing says otherwise is its maximum, and a declared power
 * above it is held to it. */

export const CHIP_MIN_DBM = -9
export const CHIP_MAX_DBM = 22
export const FEM_MAX_DBM = 27

/** A node's maximum power at the connector, in dBm, from its own `max_dbm`. */
export function maxDbm(own: number | null | undefined): number {
  return own === undefined || own === null ? CHIP_MAX_DBM : own
}

/** What a node sends at: its declared power held to its maximum, else its maximum. */
export function txDbm(own: number | null | undefined, declared: number | undefined | null): number {
  const top = maxDbm(own)
  return declared === undefined || declared === null ? top : Math.min(declared, top)
}
