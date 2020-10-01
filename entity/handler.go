package entity

import "context"

type HandlerFunction func(context.Context, *CommandRequest) error
