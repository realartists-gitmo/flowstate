// translator.h
//
// Copyright (c) 2023-2024 Junpei Kawamoto
//
// This software is released under the MIT License.
//
// http://opensource.org/licenses/mit-license.php

#pragma once

#include <memory>

#include <ctranslate2/translator.h>
#include <ctranslate2/models/model.h>

#include "rust/cxx.h"

#include "config.h"

struct VecStr;
struct TranslationOptions;
struct TranslationResult;
struct GenerationStepResult;
struct TranslationCallbackBox;

class Translator {
private:
    std::unique_ptr<ctranslate2::Translator> impl;

public:
    Translator(std::unique_ptr<ctranslate2::Translator> impl)
        : impl(std::move(impl)) { }

    rust::Vec<TranslationResult>
    translate_batch(
        const rust::Vec<VecStr>& source,
        const TranslationOptions& options,
        bool has_callback,
        TranslationCallbackBox& callback
    ) const;

    rust::Vec<TranslationResult>
    translate_batch_with_target_prefix(
        const rust::Vec<VecStr>& source,
        const rust::Vec<VecStr>& target_prefix,
        const TranslationOptions& options,
        bool has_callback,
        TranslationCallbackBox& callback
    ) const;

    inline size_t num_queued_batches() const {
        return this->impl->num_queued_batches();
    }

    inline size_t num_active_batches() const {
        return this->impl->num_active_batches();
    }

    inline size_t num_replicas() const {
        return this->impl->num_replicas();
    }
};

inline std::unique_ptr<Translator> translator(
    rust::Str model_path,
    std::unique_ptr<Config> config
) {
    // FLOWSTATE PATCH: build the ModelLoader explicitly so `num_replicas_per_device` (inter_threads)
    // can be set — the convenience Translator constructor leaves it at the default of 1, which
    // serializes all decodes through a single replica. >1 lets independent decodes run concurrently.
    ctranslate2::models::ModelLoader model_loader(static_cast<std::string>(model_path));
    model_loader.device = config->device;
    model_loader.device_indices =
        std::vector<int>(config->device_indices.begin(), config->device_indices.end());
    model_loader.compute_type = config->compute_type;
    model_loader.tensor_parallel = config->tensor_parallel;
    if (config->inter_threads > 0) {
        model_loader.num_replicas_per_device = config->inter_threads;
    }
    return std::make_unique<Translator>(std::make_unique<ctranslate2::Translator>(
        model_loader,
        *config->replica_pool_config
    ));
}
