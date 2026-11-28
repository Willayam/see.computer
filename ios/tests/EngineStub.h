#include "../core/include/see_core.h"

/* A stand-in for the Rust engine, so the checks can see how Swift calls it. */

/* Sends an event to Swift from another thread, as the engine does. */
void stub_emit(int32_t kind, uint64_t a, uint64_t b, const char *text);

/* The next feed stays inside the engine until `stub_release_feed`. */
void stub_hold_next_feed(void);
void stub_wait_until_feed_is_inside(void);
void stub_release_feed(void);

/* Calls that entered the engine while another call was still inside it. */
int stub_overlaps(void);
int stub_finishes(void);
int stub_releases(void);
