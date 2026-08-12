FROM sysg-usecase-base

COPY tests/docker/pid1/stack.yaml /etc/systemg/systemg.yaml
RUN mkdir -p /var/lib/systemg /var/log/systemg

ENTRYPOINT ["sysg", "init", "-c", "/etc/systemg/systemg.yaml"]
