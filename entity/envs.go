package entity

type GetEnvsRequest struct {
	ProjectID     string
	EnvironmentID string
	ServiceID     string
}

type GetEnvsForPluginRequest struct {
	ProjectID     string
	EnvironmentID string
	PluginID      string
}

type UpdateEnvsRequest struct {
	ProjectID     string
	EnvironmentID string
	PluginID      string
	ServiceID     string
	Envs          *Envs
}

type DeleteVariableRequest struct {
	ProjectID     string
	EnvironmentID string
	PluginID      string
	ServiceID     string
	Name          string
}

type Envs map[string]string

func (e Envs) Get(name string) string {
	return e[name]
}

func (e Envs) Set(name, value string) {
	e[name] = value
}

func (e Envs) Has(name string) bool {
	_, ok := e[name]
	return ok
}

func (e Envs) Delete(name string) {
	delete(e, name)
}
