#ifndef SEE_CORE_H
#define SEE_CORE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct SeeEngine SeeEngine;

/* Progress carries done and total in a and b. A preview carries the take
 * number from begin in a. Done carries the job number in a and 1 on failure
 * in b. The text is valid for the duration of the callback. */
enum {
  SeeEventDownloading = 0,
  SeeEventLoading = 1,
  SeeEventWarming = 2,
  SeeEventReady = 3,
  SeeEventFailed = 4,
  SeeEventPreview = 5,
  SeeEventDone = 6,
};

typedef void (*SeeCallback)(void *context, int32_t kind, uint64_t a, uint64_t b, const char *text);

/* Downloads the model if it is missing, loads it, and warms it, on its own
 * thread. Events arrive on another thread. Release a failed engine before retrying. */
SeeEngine *see_engine_start(const char *models, const char *dictionary, bool previews,
                            void *context, SeeCallback callback);
void see_engine_release(SeeEngine *engine);

/* One take: begin, any number of feeds, then finish or discard. Samples are
 * 16 kHz mono floats and are copied before the call returns. */
uint64_t see_engine_begin(SeeEngine *engine);
void see_engine_feed(SeeEngine *engine, const float *samples, size_t count);
uint64_t see_engine_finish(SeeEngine *engine, const float *samples, size_t count);
void see_engine_discard(SeeEngine *engine);

/* Pages the weights back in if iOS reclaimed them. */
void see_engine_warm(SeeEngine *engine);

#endif
