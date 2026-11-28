#include "EngineStub.h"

#include <dispatch/dispatch.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

static SeeCallback callback;
static void *context;
static atomic_int inside, overlaps, finishes, releases;
static atomic_bool hold;
static atomic_uint_fast64_t utterance, job;
static dispatch_semaphore_t entered, released;

static void setup(void) {
  entered = dispatch_semaphore_create(0);
  released = dispatch_semaphore_create(0);
}

static void enter(void) {
  if (atomic_fetch_add(&inside, 1) != 0) atomic_fetch_add(&overlaps, 1);
}

static void leave(void) { atomic_fetch_sub(&inside, 1); }

SeeEngine *see_engine_start(const char *models, const char *dictionary, bool previews,
                            void *caller, SeeCallback events) {
  static pthread_once_t once = PTHREAD_ONCE_INIT;
  pthread_once(&once, setup);
  callback = events;
  context = caller;
  return (SeeEngine *)&callback;
}

void see_engine_release(SeeEngine *engine) { atomic_fetch_add(&releases, 1); }
int stub_releases(void) { return atomic_load(&releases); }

uint64_t see_engine_begin(SeeEngine *engine) {
  enter();
  uint64_t number = atomic_fetch_add(&utterance, 1) + 1;
  leave();
  return number;
}

void see_engine_feed(SeeEngine *engine, const float *samples, size_t count) {
  enter();
  if (atomic_exchange(&hold, false)) {
    dispatch_semaphore_signal(entered);
    dispatch_semaphore_wait(released, DISPATCH_TIME_FOREVER);
  }
  leave();
}

uint64_t see_engine_finish(SeeEngine *engine, const float *samples, size_t count) {
  enter();
  atomic_fetch_add(&finishes, 1);
  uint64_t number = atomic_fetch_add(&job, 1) + 1;
  leave();
  return number;
}

void see_engine_discard(SeeEngine *engine) {
  enter();
  leave();
}

void see_engine_warm(SeeEngine *engine) {
  enter();
  leave();
}

struct event {
  int32_t kind;
  uint64_t a, b;
  char *text;
};

static void *send_event(void *raw) {
  struct event *event = raw;
  callback(context, event->kind, event->a, event->b, event->text);
  free(event->text);
  free(event);
  return NULL;
}

void stub_emit(int32_t kind, uint64_t a, uint64_t b, const char *text) {
  struct event *event = malloc(sizeof *event);
  *event = (struct event){kind, a, b, strdup(text)};
  pthread_t thread;
  pthread_create(&thread, NULL, send_event, event);
  pthread_join(thread, NULL);
}

void stub_hold_next_feed(void) { atomic_store(&hold, true); }
void stub_wait_until_feed_is_inside(void) { dispatch_semaphore_wait(entered, DISPATCH_TIME_FOREVER); }
void stub_release_feed(void) { dispatch_semaphore_signal(released); }
int stub_overlaps(void) { return atomic_load(&overlaps); }
int stub_finishes(void) { return atomic_load(&finishes); }
