package entity

type WorkflowStatus string

func (s WorkflowStatus) IsError() bool {
	return s == "Error"
}

func (s WorkflowStatus) IsRunning() bool {
	return s == "Running"
}

func (s WorkflowStatus) IsComplete() bool {
	return s == "Complete"
}

var (
	WorkflowRunning  WorkflowStatus = "Running"
	WorkflowComplete WorkflowStatus = "Complete"
	WorkflowError    WorkflowStatus = "Error"
)

type WorkflowStatusResponse struct {
	Status WorkflowStatus `json:"status"`
}
