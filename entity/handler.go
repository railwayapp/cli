package entity

import "context"

type HandlerFunction func(context.Context, *CommandRequest) error

type PanicFunction func(context.Context, interface{}) error
