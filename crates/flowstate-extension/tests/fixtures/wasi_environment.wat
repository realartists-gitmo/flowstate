(component
  (type $environment-type (instance
    (type $get-environment-type (func (result (list (tuple string string)))))
    (type $get-arguments-type (func (result (list string))))
    (type $initial-cwd-type (func (result (option string))))
    (export "get-environment" (func (type $get-environment-type)))
    (export "get-arguments" (func (type $get-arguments-type)))
    (export "initial-cwd" (func (type $initial-cwd-type)))
  ))
  (import "wasi:cli/environment@0.2.9" (instance $environment (type $environment-type)))

  (type $host-type (instance
    (type $status-type (func (param "message" string)))
    (export "set-status" (func (type $status-type)))
  ))
  (import "flowstate:extension/host@1.0.0" (instance $host (type $host-type)))
  (alias export $host "set-status" (func $status))

  (core module $memory-module
    (memory (export "memory") 1)
    (data (i32.const 32) "called")
  )
  (core instance $memory-instance (instantiate $memory-module))
  (alias core export $memory-instance "memory" (core memory $memory))
  (core func $status-core
    (canon lower (func $status) (memory $memory) string-encoding=utf8)
  )
  (core instance $imports
    (export "memory" (memory $memory))
    (export "set-status" (func $status-core))
  )

  (core module $guest
    (import "imports" "memory" (memory 1))
    (import "imports" "set-status" (func $status (param i32 i32)))
    (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32) i32.const 1024)
    (func (export "run") (param i32 i32) (result i32)
      i32.const 32
      i32.const 6
      call $status
      i32.const 2048
      i32.const 0
      i32.store
      i32.const 2048
    )
  )
  (core instance $guest-instance (instantiate $guest (with "imports" (instance $imports))))
  (alias core export $guest-instance "cabi_realloc" (core func $realloc))
  (alias core export $guest-instance "run" (core func $run-core))

  (type $run-type (func (param "action-id" string) (result (result (error string)))))
  (func $run (type $run-type)
    (canon lift (core func $run-core) (memory $memory) (realloc $realloc) string-encoding=utf8)
  )
  (export "run" (func $run))
)
