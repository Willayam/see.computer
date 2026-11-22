// Mac runner for the JNI-free LatinIME proof build.
#include "dictionary/property/ngram_context.h"
#include "dictionary/structure/dictionary_structure_with_buffer_policy_factory.h"
#include "suggest/core/dictionary/dictionary.h"
#include "suggest/core/layout/proximity_info.h"
#include "suggest/core/result/suggestion_results.h"
#include "suggest/core/session/dic_traverse_session.h"
#include "suggest/core/suggest_options.h"
#include "utils/autocorrection_threshold_utils.h"
#import <Foundation/Foundation.h>
#include <algorithm>
#include <cmath>
#include <filesystem>
#include <vector>
using namespace latinime;

static id readJSON(const char *path) {
  NSData *data = [NSData dataWithContentsOfFile:@(path)];
  return [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
}

int main(int argc, const char **argv) {
  if (argc != 5)
    return 2;
  @autoreleasepool {
    NSDictionary *frames = readJSON(argv[1]);
    NSArray *sentences = readJSON(argv[2]);
    auto bytes = std::filesystem::file_size(argv[3]);
    auto policy = DictionaryStructureWithBufferPolicyFactory::
        newPolicyForExistingDictFile(argv[3], 0, bytes, false);
    if (!policy)
      return 1;
    Dictionary dictionary(std::move(policy));
    int x[64], y[64], w[64], h[64], c[64], count = 0;
    float cx[64], cy[64], radius[64];
    for (NSString *key in
         [frames.allKeys sortedArrayUsingSelector:@selector(compare:)]) {
      NSArray *f = frames[key];
      int i = count++;
      cx[i] = [f[0] floatValue];
      cy[i] = [f[1] floatValue];
      w[i] = lround([f[2] doubleValue]);
      h[i] = lround([f[3] doubleValue]);
      x[i] = lround(cx[i] - w[i] / 2.0);
      y[i] = lround(cy[i] - h[i] / 2.0);
      c[i] = [key isEqual:@"Space"] ? 32 : [key characterAtIndex:0];
      radius[i] = 0.15 * hypot(w[i], h[i]);
    }
    constexpr int cols = 40, rows = 21;
    int grid[rows * cols * MAX_PROXIMITY_CHARS_SIZE];
    std::fill_n(grid, std::size(grid), NOT_A_CODE_POINT);
    int pitch = lround([frames[@"q"][2] doubleValue]);
    for (int j = 0; j < rows; ++j)
      for (int i = 0; i < cols; ++i) {
        std::vector<std::pair<double, int>> near;
        for (int k = 0; k < count; ++k) {
          double dx = std::max(0., std::abs(i * 10 + 5. - cx[k]) - w[k] / 2.);
          double dy = std::max(0., std::abs(j * 10 + 5. - cy[k]) - h[k] / 2.);
          if (hypot(dx, dy) < pitch)
            near.emplace_back(hypot(dx, dy), c[k]);
        }
        std::sort(near.begin(), near.end());
        for (size_t k = 0; k < near.size() && k < MAX_PROXIMITY_CHARS_SIZE; ++k)
          grid[(j * cols + i) * MAX_PROXIMITY_CHARS_SIZE + k] = near[k].second;
      }
    ProximityInfo layout(394, 210, cols, rows, pitch, 53, grid, count, x, y, w,
                         h, c, cx, cy, radius);
    int opts[] = {0, 1, 1, 0, 1000};
    SuggestOptions options(opts, 5);
    NSMutableArray *output = [NSMutableArray array];
    for (NSDictionary *s in sentences)
      @autoreleasepool {
        NSString *previous = @"";
        NSMutableArray *commits = [NSMutableArray array],
                       *bars = [NSMutableArray array];
        for (NSArray *points in s[@"taps"]) {
          int in[MAX_WORD_LENGTH]{}, tx[MAX_WORD_LENGTH]{},
              ty[MAX_WORD_LENGTH]{};
          int times[MAX_WORD_LENGTH]{}, pointers[MAX_WORD_LENGTH]{},
              prev[MAX_WORD_LENGTH]{}, n = 0;
          if (points.count >= MAX_WORD_LENGTH ||
              previous.length >= MAX_WORD_LENGTH)
            return 3;
          NSMutableString *raw = [NSMutableString string];
          for (NSArray *point in points) {
            double px = [point[0] doubleValue], py = [point[1] doubleValue],
                   best = INFINITY;
            for (int k = 0; k < count; ++k)
              if (c[k] != 32 && hypot(px - cx[k], py - cy[k]) < best) {
                best = hypot(px - cx[k], py - cy[k]);
                in[n] = c[k];
              }
            [raw appendFormat:@"%C", (unichar)in[n]];
            tx[n] = std::clamp((int)lround(px), 0, 393);
            ty[n] = std::clamp((int)lround(py), 0, 209);
            times[n] = n * 100;
            ++n;
          }
          for (NSUInteger i = 0; i < previous.length; i++)
            prev[i] = [previous characterAtIndex:i];
          NgramContext context(prev, (int)previous.length,
                               previous.length == 0);
          DicTraverseSession session(bytes >= 256 * 1024);
          SuggestionResults results(18);
          if (n)
            dictionary.getSuggestions(
                &layout, &session, tx, ty, times, pointers, in, n, &context,
                &options, NOT_A_WEIGHT_OF_LANG_MODEL_VS_SPATIAL_MODEL,
                &results);
          NSMutableArray *candidates = [NSMutableArray array];
          // LatinIME 127336e9, java/src/com/android/inputmethod/latin/Suggest.java, getSuggestedWordsForNonBatchInput.
          bool typedSuggested = false, eligible = false, allowlisted = false;
          for (auto &word : results.sortedWords()) {
            NSMutableString *value = [NSMutableString string];
            for (int i = 0; i < word.getCodePointCount(); i++)
              [value appendFormat:@"%C", (unichar)word.getCodePoint()[i]];
            if ([value isEqual:raw]) {
              typedSuggested = true;
              continue;
            }
            allowlisted |= (word.getType() & 255) == 3;
            if ([candidates containsObject:value])
              continue;
            if (!candidates.count) {
              const int kind = word.getType() & 255;
              const float score =
                  AutocorrectionThresholdUtils::calcNormalizedScore(
                      in, n, word.getCodePoint(), word.getCodePointCount(),
                      word.getScore());
              eligible =
                  kind == 3 || (kind != 7 && (word.getType() & 0x10000000) &&
                                score >= 0.185);
            }
            [candidates addObject:value];
          }
          NSString *chosen =
              eligible && (allowlisted || (!typedSuggested && n > 1))
                  ? candidates.firstObject
                  : raw;
          NSMutableArray *bar = [NSMutableArray arrayWithObject:raw];
          for (NSString *v in candidates)
            if (bar.count < 3)
              [bar addObject:v];
          [commits addObject:chosen];
          [bars addObject:bar];
          previous = [chosen componentsSeparatedByString:@" "].lastObject;
        }
        [output addObject:@{
          @"id" : s[@"id"],
          @"commits" : commits,
          @"bars" : bars,
          @"text" : [commits componentsJoinedByString:@" "]
        }];
      }
    NSData *data = [NSJSONSerialization dataWithJSONObject:output
                                                   options:0
                                                     error:nil];
    return [data writeToFile:@(argv[4]) atomically:YES] ? 0 : 1;
  }
}
